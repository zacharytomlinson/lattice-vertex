//! One resident radio task. Driver futures suspend through task notifications;
//! no executor thread, allocation, or graph scheduling is introduced.

use super::{
    receive::{finish_frame_ticks, receive_turn_ticks, ReceiveBuffer},
    Error, MAX_FRAME_SIZE,
};
use crate::{
    kernel::tasks,
    port,
    sync::{TaskContext, TaskHandle},
    types::*,
};
use core::{
    ffi::c_void,
    future::Future,
    marker::PhantomData,
    pin::pin,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

// The peer frame reserves about 5 KiB on Nordic before callees and exception
// frames; leave fixed headroom for native dispatch and attached-host operations.
static mut STACK: [StackType_t; 2048] = [0; 2048];
static mut CONTROL: tasks::StaticTask_t = tasks::StaticTask_t::new();
static mut PERIPHERAL: Option<port::RadioPeripheral> = None;
static WAKE_TARGET: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static mut NATIVE: Option<&'static dyn crate::kernel::appliance::Native> = None;
static mut DESCRIPTION: Option<crate::kernel::peer::exposure::Description> = None;

pub(crate) fn start(
    context: &TaskContext,
    peripheral: port::RadioPeripheral,
    description: crate::kernel::peer::exposure::Description,
    native: &'static dyn crate::kernel::appliance::Native,
) {
    assert_eq!(
        tasks::xTaskGetSchedulerState(),
        tasks::taskSCHEDULER_NOT_STARTED
    );
    unsafe {
        assert!(PERIPHERAL.is_none(), "radio task already installed");
        PERIPHERAL = Some(peripheral);
        DESCRIPTION = Some(description);
        NATIVE = Some(native);
        TaskHandle::spawn_static(context, b"radio", &mut STACK, &mut CONTROL, 3, run).unwrap();
    }
}

// All wakers refer to the one resident kernel task. Its TCB is static, the task
// never exits, and no deletion handle escapes startup. Wakeups signal hardware
// availability, not distinct graph deltas; packet bytes remain in driver buffers.
unsafe fn wake(data: *const ()) {
    let task = data as TaskHandle_t;
    if port::xPortIsInsideInterrupt() != pdFALSE {
        #[cfg(feature = "test-support")]
        super::testing::event(super::testing::Event::InterruptWake);
        let mut woken = pdFALSE;
        tasks::xTaskGenericNotifyFromISR(
            task,
            0,
            1,
            tasks::eNotifyAction::eSetBits as i32,
            ptr::null_mut(),
            &mut woken,
        );
        port::portYIELD_FROM_ISR(woken);
    } else {
        tasks::xTaskGenericNotify(
            task,
            0,
            1,
            tasks::eNotifyAction::eSetBits as i32,
            ptr::null_mut(),
        );
    }
}
unsafe fn clone_waker(data: *const ()) -> RawWaker {
    RawWaker::new(data, &WAKER_VTABLE)
}
unsafe fn drop_waker(_: *const ()) {}
static WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(clone_waker, wake, wake, drop_waker);

fn local_pending() -> bool {
    #[cfg(feature = "host-link")]
    if crate::kernel::peer::host::pending() {
        return true;
    }
    crate::kernel::change::pending()
}

#[derive(Clone, Copy)]
enum WaitMode {
    Hardware,
    Receive { local_work: bool },
}

fn wait<F: Future>(
    future: F,
    waker: &Waker,
    timeout: TickType_t,
    mode: WaitMode,
) -> Option<F::Output> {
    let start = tasks::xTaskGetTickCount();
    let mut finish_started = None;
    let mut future = pin!(future);
    let mut cx = Context::from_waker(waker);
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return Some(result);
        }
        let interrupted = matches!(mode, WaitMode::Receive { local_work: true }) && local_pending();
        let now = tasks::xTaskGetTickCount();
        let mut remaining = if timeout == portMAX_DELAY {
            timeout
        } else {
            timeout.saturating_sub(now.wrapping_sub(start))
        };
        if interrupted || remaining == 0 {
            let receiving =
                matches!(mode, WaitMode::Receive { .. }) && port::Radio::receiving_frame();
            remaining = finish_frame_ticks(
                &mut finish_started,
                now as u32,
                receiving,
                receive_turn_ticks(crate::configTICK_RATE_HZ as u32),
            )? as TickType_t;
        }
        // A notification between poll and wait is retained by FreeRTOS, avoiding
        // a missed IRQ. A stale notification only causes another driver poll.
        #[cfg(feature = "test-support")]
        super::testing::event(super::testing::Event::NotificationWait);
        unsafe {
            tasks::xTaskNotifyWait(0, u32::MAX, ptr::null_mut(), remaining);
        }
    }
}

/// Selected driver and its resident task waker. Constructed only inside `run`;
/// hardware fixtures receive a scoped borrow through the test-only entry.
pub struct Transport {
    radio: port::Radio,
    peer_runtime: bool,
    received: ReceiveBuffer,
    waker: Waker,
    _task_context: PhantomData<*mut ()>,
}

pub(crate) fn notify_work() {
    let target = WAKE_TARGET.load(Ordering::Acquire);
    if !target.is_null() {
        unsafe {
            wake(target.cast());
        }
    }
}

impl Transport {
    /// Formation uses its own receive windows. Resident traffic also gives
    /// neighbors a receive turn between transmissions and accepts local work.
    pub(crate) fn enable_peer_runtime(&mut self) {
        self.peer_runtime = true;
    }

    pub fn address(&self) -> u64 {
        self.radio.address()
    }

    pub fn configure(&mut self, channel: u8, pan_id: u16) -> Result<(), Error> {
        self.received.clear();
        self.radio.configure(channel, pan_id)
    }

    /// Peak channel energy in dBm over the requested receive-only window.
    /// Both ports cancel a timed-out scan before another operation can start.
    pub fn energy_detect(&mut self, duration_us: u16) -> Result<i8, Error> {
        let timeout =
            crate::milliseconds_to_ticks_checked(u64::from(duration_us).div_ceil(1000) + 20)
                .unwrap();
        wait(
            self.radio.energy_detect(duration_us),
            &self.waker,
            timeout,
            WaitMode::Hardware,
        )
        .unwrap_or(Err(Error::Aborted))
    }

    pub fn receive(
        &mut self,
        frame: &mut [u8; MAX_FRAME_SIZE],
        timeout: TickType_t,
    ) -> Option<Result<usize, Error>> {
        if let Some(length) = self.received.pop(frame) {
            return Some(Ok(length));
        }
        wait(
            self.radio.receive(frame),
            &self.waker,
            timeout,
            if self.peer_runtime {
                WaitMode::Receive { local_work: true }
            } else {
                WaitMode::Hardware
            },
        )
    }

    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), Error> {
        // No timeout drops active DMA. The driver retains its buffer until the
        // hardware completes or aborts this single attempt.
        let result = wait(
            self.radio.transmit(frame),
            &self.waker,
            portMAX_DELAY,
            WaitMode::Hardware,
        )
        .unwrap();
        if result.is_ok() && self.peer_runtime {
            self.receive_turn();
        }
        result
    }

    fn receive_turn(&mut self) {
        let start = tasks::xTaskGetTickCount();
        let window = receive_turn_ticks(crate::configTICK_RATE_HZ as u32) as TickType_t;
        let mut frame = [0; MAX_FRAME_SIZE];
        loop {
            let remaining = window.saturating_sub(tasks::xTaskGetTickCount().wrapping_sub(start));
            if remaining == 0 {
                return;
            }
            // Pending local changes must not cancel this receive opportunity.
            // Retain the first valid frame for ordinary dispatch after send returns.
            match wait(
                self.radio.receive(&mut frame),
                &self.waker,
                remaining,
                WaitMode::Receive { local_work: false },
            ) {
                Some(Ok(length)) => {
                    let evicted = self.received.push(&frame[..length]);
                    #[cfg(feature = "test-support")]
                    if evicted {
                        super::testing::event(super::testing::Event::ReceiveEvicted);
                    }
                    let _ = evicted;
                    return;
                }
                Some(Err(_)) => {
                    #[cfg(feature = "test-support")]
                    super::testing::event(super::testing::Event::ReceiveFailed);
                }
                None => return,
            }
        }
    }
}

extern "C" fn run(_: *mut c_void) {
    WAKE_TARGET.store(tasks::xTaskGetCurrentTaskHandle(), Ordering::Release);
    let peripheral = unsafe { PERIPHERAL.take().unwrap() };
    let radio = port::Radio::new(peripheral);
    let waker = unsafe {
        Waker::from_raw(RawWaker::new(
            tasks::xTaskGetCurrentTaskHandle().cast(),
            &WAKER_VTABLE,
        ))
    };
    let mut transport = Transport {
        radio,
        peer_runtime: false,
        received: ReceiveBuffer::new(),
        waker,
        _task_context: PhantomData,
    };

    #[cfg(feature = "test-support")]
    super::testing::run(&mut transport);

    crate::kernel::peer::run(
        &mut transport,
        unsafe { DESCRIPTION.take().unwrap() },
        unsafe { NATIVE.take().unwrap() },
    )
}
