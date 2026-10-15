//! One resident radio task. Driver futures suspend through task notifications;
//! no executor thread, allocation, or graph scheduling is introduced.

use super::{Error, MAX_FRAME_SIZE};
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

static mut STACK: [StackType_t; 1536] = [0; 1536];
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

fn wait<F: Future>(
    future: F,
    waker: &Waker,
    timeout: TickType_t,
    local_work: bool,
) -> Option<F::Output> {
    let start = tasks::xTaskGetTickCount();
    let mut future = pin!(future);
    let mut cx = Context::from_waker(waker);
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return Some(result);
        }
        if local_work && local_pending() {
            return None;
        }
        let remaining = if timeout == portMAX_DELAY {
            timeout
        } else {
            timeout.saturating_sub(tasks::xTaskGetTickCount().wrapping_sub(start))
        };
        if remaining == 0 {
            return None;
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
    local_work: bool,
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
    pub(crate) fn enable_local_work(&mut self) {
        self.local_work = true;
    }

    pub fn address(&self) -> u64 {
        self.radio.address()
    }

    pub fn configure(&mut self, channel: u8, pan_id: u16) -> Result<(), Error> {
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
            false,
        )
        .unwrap_or(Err(Error::Aborted))
    }

    pub fn receive(
        &mut self,
        frame: &mut [u8; MAX_FRAME_SIZE],
        timeout: TickType_t,
    ) -> Option<Result<usize, Error>> {
        wait(
            self.radio.receive(frame),
            &self.waker,
            timeout,
            self.local_work,
        )
    }

    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), Error> {
        // No timeout drops active DMA. The driver retains its buffer until the
        // hardware completes or aborts this single attempt.
        wait(
            self.radio.transmit(frame),
            &self.waker,
            portMAX_DELAY,
            false,
        )
        .unwrap()
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
        local_work: false,
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
