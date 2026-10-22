//! One resident radio task. Driver futures suspend through task notifications;
//! no executor thread, allocation, or graph scheduling is introduced.

use super::{
    cost::{Samples, Transmission},
    mac,
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

// Resident forwarding plus callees/interrupt frames exceeded the former 8 KiB
// reservation in the guarded C6 check. Keep 12 KiB for ordinary participation.
// Ed25519 verification nests field arithmetic above peer dispatch. The 16 KiB
// update stack overflowed on C6 during a valid remote offer; reserve 24 KiB.
const STACK_WORDS: usize = if cfg!(feature = "firmware-update") {
    6144
} else {
    3072
};
static mut STACK: [StackType_t; STACK_WORDS] = [0; STACK_WORDS];
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
    #[cfg(all(
        feature = "firmware-update",
        any(
            all(feature = "port-nrf52840", feature = "nrf52840-update"),
            all(feature = "port-esp32c6", feature = "esp32c6-update")
        )
    ))]
    if crate::kernel::update::runtime::pending() {
        return true;
    }
    #[cfg(feature = "host-link")]
    if crate::kernel::peer::host::pending() {
        return true;
    }
    crate::kernel::change::pending() || crate::kernel::appliance::execution::completion_pending()
}

#[derive(Clone, Copy)]
enum WaitMode {
    Hardware,
    Transmit,
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
    let mut ack_started = None;
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
        if matches!(mode, WaitMode::Transmit) && port::Radio::waiting_for_ack() {
            let since = *ack_started.get_or_insert(now);
            let ticks = (u64::from(mac::ACK_WAIT_US) * crate::configTICK_RATE_HZ as u64)
                .div_ceil(1_000_000) as TickType_t
                + 1;
            remaining = remaining.min(ticks.saturating_sub(now.wrapping_sub(since)));
        }
        if interrupted || remaining == 0 {
            let receiving = matches!(mode, WaitMode::Receive { .. } | WaitMode::Transmit)
                && port::Radio::receiving_frame();
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
    duplicates: mac::Duplicates,
    backoff: mac::Backoff,
    pan: u16,
    costs: Samples,
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

/// Watchdog reloads require task progress. The timer IRQ never feeds it.
pub(crate) fn watchdog_progress(radio_completed: bool) {
    #[cfg(any(feature = "nrf52840-update", feature = "esp32c6-update"))]
    {
        if crate::kernel::storage::startup_progress(radio_completed) {
            return;
        }
        #[cfg(feature = "firmware-update")]
        crate::kernel::update::runtime::startup_progress(radio_completed);
        // Local boot fixtures intentionally retain explicit confirmation.
        #[cfg(not(feature = "firmware-update"))]
        port::watchdog::feed();
    }
    let _ = radio_completed;
}

impl Transport {
    pub(crate) fn pending_receive(&self) -> bool {
        self.received.pending() || port::Radio::receiving_frame()
    }
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
        self.costs.clear();
        self.duplicates = mac::Duplicates::new();
        self.pan = pan_id;
        self.radio.configure(channel, pan_id)
    }

    /// Peak channel energy in dBm over the requested receive-only window.
    /// Both ports cancel a timed-out scan before another operation can start.
    pub fn energy_detect(&mut self, duration_us: u16) -> Result<i8, Error> {
        let timeout =
            crate::milliseconds_to_ticks_checked(u64::from(duration_us).div_ceil(1000) + 20)
                .unwrap();
        let result = wait(
            self.radio.energy_detect(duration_us),
            &self.waker,
            timeout,
            WaitMode::Hardware,
        )
        .unwrap_or(Err(Error::Aborted));
        watchdog_progress(result.is_ok());
        result
    }

    pub fn receive(
        &mut self,
        frame: &mut [u8; MAX_FRAME_SIZE],
        timeout: TickType_t,
    ) -> Option<Result<usize, Error>> {
        let start = tasks::xTaskGetTickCount();
        loop {
            let result = if let Some(length) = self.received.pop(frame) {
                Some(Ok(length))
            } else {
                let remaining =
                    timeout.saturating_sub(tasks::xTaskGetTickCount().wrapping_sub(start));
                if remaining == 0 {
                    return None;
                }
                wait(
                    self.radio.receive(frame),
                    &self.waker,
                    remaining,
                    if self.peer_runtime {
                        WaitMode::Receive { local_work: true }
                    } else {
                        WaitMode::Hardware
                    },
                )
            };
            match result {
                Some(Ok(length)) if !self.accept_received(&frame[..length]) => continue,
                _ => return result,
            }
        }
    }

    fn accept_received(&mut self, frame: &[u8]) -> bool {
        if mac::is_ack(frame) {
            return false;
        }
        if mac::ack_for(frame, self.pan, self.address()).is_none() {
            return true;
        }
        let ms =
            ((tasks::xTaskGetTickCount() as u64 * 1000) / crate::configTICK_RATE_HZ as u64) as u32;
        self.duplicates.accept(frame, ms)
    }

    pub(crate) fn take_cost_sample(&mut self) -> Option<Transmission> {
        self.costs.take()
    }

    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), Error> {
        let started = tasks::xTaskGetTickCount();
        let acknowledged = mac::requests_ack(frame);
        let attempts = if acknowledged { mac::ATTEMPTS } else { 1 };
        let mut result = Err(Error::Aborted);
        let mut unconfirmed = false;
        for attempt in 0..attempts {
            if attempt != 0 {
                let delay = self
                    .backoff
                    .ticks(attempt - 1, crate::configTICK_RATE_HZ as u32);
                self.capture_for(delay as TickType_t, false);
            }
            // The port performs one TX/ACK exchange. Its cancellation guard
            // stops DMA before a bounded deadline can return its storage.
            result = wait(
                self.radio.transmit(frame, &mut self.received),
                &self.waker,
                if acknowledged {
                    mac::attempt_ticks(crate::configTICK_RATE_HZ as u32) as TickType_t
                } else {
                    portMAX_DELAY
                },
                WaitMode::Transmit,
            )
            .unwrap_or(Err(Error::NoAcknowledgement));
            unconfirmed |= result == Err(Error::NoAcknowledgement);
            if result.is_ok()
                || !matches!(result, Err(Error::NoAcknowledgement | Error::ChannelBusy))
            {
                break;
            }
        }
        if result.is_ok() && self.peer_runtime {
            self.receive_turn();
        }
        if acknowledged && self.peer_runtime {
            let ticks = tasks::xTaskGetTickCount().wrapping_sub(started).max(1);
            let us = (u64::from(ticks) * 1_000_000 / crate::configTICK_RATE_HZ as u64)
                .min(u64::from(u32::MAX)) as u32;
            self.costs.push(Transmission {
                peer: u64::from_le_bytes(frame[5..13].try_into().unwrap()),
                elapsed_us: us,
                acknowledged_bytes: if result.is_ok() {
                    (frame.len() - 21) as u16
                } else {
                    0
                },
            });
        }
        if result.is_err() && unconfirmed {
            Err(Error::NoAcknowledgement)
        } else {
            result
        }
    }

    fn receive_turn(&mut self) {
        self.capture_for(
            receive_turn_ticks(crate::configTICK_RATE_HZ as u32) as TickType_t,
            true,
        );
    }

    fn capture_for(&mut self, window: TickType_t, first_only: bool) {
        let start = tasks::xTaskGetTickCount();
        let mut frame = [0; MAX_FRAME_SIZE];
        loop {
            let remaining = window.saturating_sub(tasks::xTaskGetTickCount().wrapping_sub(start));
            if remaining == 0 {
                return;
            }
            // Pending local changes must not cancel this receive opportunity.
            // Retain frames for ordinary dispatch; receive turns end at the first,
            // while retry backoff listens through the entire chosen interval.
            match wait(
                self.radio.receive(&mut frame),
                &self.waker,
                remaining,
                WaitMode::Receive { local_work: false },
            ) {
                Some(Ok(length)) => {
                    if mac::is_ack(&frame[..length]) {
                        continue;
                    }
                    let evicted = self.received.push(&frame[..length]);
                    #[cfg(feature = "test-support")]
                    if evicted {
                        super::testing::event(super::testing::Event::ReceiveEvicted);
                    }
                    let _ = evicted;
                    if first_only {
                        return;
                    }
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
    // Restore on the resident task's stack, before PHY startup/formation and
    // before a trial can be confirmed. This requires no peer or physical sample.
    crate::kernel::program::runtime::poll();
    let peripheral = unsafe { PERIPHERAL.take().unwrap() };
    let radio = port::Radio::new(peripheral);
    let waker = unsafe {
        Waker::from_raw(RawWaker::new(
            tasks::xTaskGetCurrentTaskHandle().cast(),
            &WAKER_VTABLE,
        ))
    };
    let address = radio.address();
    let mut transport = Transport {
        radio,
        peer_runtime: false,
        received: ReceiveBuffer::new(),
        duplicates: mac::Duplicates::new(),
        backoff: mac::Backoff::new(address),
        pan: 0,
        costs: Samples::new(),
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
