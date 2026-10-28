//! Physical acquisition sources, pending readiness and the existing acquisition task.
//! Readiness coalesces; acquired deltas retain their ordinary individual records.
use super::Native;
use crate::{kernel::tasks, port, sync::TaskContext, types::*};
use core::{
    ffi::c_void,
    num::NonZeroU32,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
    task::{RawWaker, RawWakerVTable, Waker},
};
const COUNT: usize = crate::configLATTICE_NATIVE_STATES;
const _: () = assert!(COUNT <= 32);

/// The physical reason to acquire a state. This is local hardware configuration,
/// not part of its mathematical surface or distributed program definition.
#[derive(Clone, Copy)]
pub enum Acquisition {
    /// A peripheral event wakes acquisition. Several states may share one event.
    Event(&'static AcquisitionEvent),
    /// Minimum delay after an acquisition before the next timed sample.
    /// Use `Acquisition::periodic` to validate the wrap-safe interval.
    Periodic(NonZeroU32),
    /// Acquire at startup and around native calls, with no background sampling.
    /// Intermediate/external changes need an event or periodic source instead.
    OnInvocation,
}
impl Acquisition {
    /// Construct a nonzero interval, at most i32::MAX milliseconds.
    /// Hardware that samples faster than the kernel tick should retain samples
    /// through its own DMA/counter/peripheral binding.
    pub const fn periodic(milliseconds: u32) -> Self {
        assert!(
            milliseconds <= i32::MAX as u32,
            "acquisition interval exceeds wrapping clock bound"
        );
        match NonZeroU32::new(milliseconds) {
            Some(interval) => Self::Periodic(interval),
            None => panic!("zero acquisition interval"),
        }
    }
}

/// A peripheral's readiness notification, optionally shared by several getters.
/// Bind it with `State::acquisition`; give `waker()` to an interrupt-capable HAL,
/// or call `notify()` from the device ISR. No value or network message is supplied.
/// Events before binding are covered by the startup baseline, not replayed.
///
/// Configure interrupts that notify this event at a kernel-callable priority.
/// On Nordic these are priorities 5..=7. A notification is not an observation:
/// preserve intervening samples in peripheral/FIFO storage when they matter.
pub struct AcquisitionEvent {
    owner: AtomicPtr<Signals>,
    states: AtomicU32,
}
impl AcquisitionEvent {
    /// An initially unbound event, suitable for static storage or a passive HAL field.
    pub const fn new() -> Self {
        Self {
            owner: AtomicPtr::new(ptr::null_mut()),
            states: AtomicU32::new(0),
        }
    }
    /// Mark the bound state/group ready for acquisition, from task or ISR context.
    /// Repeated readiness coalesces until the kernel reads the source.
    pub fn notify(&self) {
        let owner = self.owner.load(Ordering::Acquire);
        if !owner.is_null() {
            // bind accepts only a static owner; its task and storage are never deleted.
            unsafe {
                (&*owner).request(self.states.load(Ordering::Acquire));
            }
        }
    }
    /// A static, allocation-free waker for the underlying hardware driver.
    pub fn waker(&'static self) -> Waker {
        unsafe { Waker::from_raw(RawWaker::new(ptr::from_ref(self).cast(), &EVENT_WAKER)) }
    }
    fn bind(&'static self, signals: &'static Signals, states: u32) {
        let target = ptr::from_ref(signals).cast_mut();
        let old = self.owner.load(Ordering::Acquire);
        assert!(
            old.is_null() || old == target,
            "acquisition event belongs to another appliance"
        );
        self.states.fetch_or(states, Ordering::AcqRel);
        match self.owner.compare_exchange(
            ptr::null_mut(),
            target,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(old) => assert_eq!(old, target, "acquisition event already bound"),
        }
    }
}
impl Default for AcquisitionEvent {
    fn default() -> Self {
        Self::new()
    }
}
unsafe fn clone_event(data: *const ()) -> RawWaker {
    RawWaker::new(data, &EVENT_WAKER)
}
unsafe fn wake_event(data: *const ()) {
    (&*data.cast::<AcquisitionEvent>()).notify();
}
unsafe fn drop_event(_: *const ()) {}
static EVENT_WAKER: RawWakerVTable =
    RawWakerVTable::new(clone_event, wake_event, wake_event, drop_event);

pub(crate) struct Signals {
    pending: AtomicU32,
    task: AtomicPtr<c_void>,
}
impl Signals {
    pub const fn new() -> Self {
        Self {
            pending: AtomicU32::new(0),
            task: AtomicPtr::new(ptr::null_mut()),
        }
    }
    pub fn request(&self, mask: u32) {
        if mask == 0 {
            return;
        }
        self.pending.fetch_or(mask, Ordering::AcqRel);
        let task = self.task.load(Ordering::Acquire);
        if task.is_null() {
            return;
        }
        unsafe {
            if port::xPortIsInsideInterrupt() != pdFALSE {
                // BASEPRI ports must reject an IRQ that can preempt kernel list
                // mutation before entering the raw FreeRTOS notification API.
                #[cfg(any(
                    feature = "port-cortex-m3",
                    feature = "port-cortex-m4f",
                    feature = "port-cortex-m7",
                    feature = "port-nrf52840"
                ))]
                {
                    let exception: u32;
                    core::arch::asm!("mrs {}, IPSR", out(reg) exception, options(nomem, nostack, preserves_flags));
                    let exception = (exception & 0x1ff) as usize;
                    assert!(
                        exception >= 16,
                        "acquisition event requires a peripheral ISR"
                    );
                    let priority =
                        ptr::read_volatile((0xe000_e400usize + exception - 16) as *const u8);
                    assert!(
                        u32::from(priority) >= crate::configMAX_SYSCALL_INTERRUPT_PRIORITY,
                        "acquisition IRQ priority cannot enter the kernel"
                    );
                }
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
    }
    pub(super) fn take(&self) -> u32 {
        self.pending.swap(0, Ordering::AcqRel)
    }
    pub(super) fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) != 0
    }
    pub(super) fn retain(&self, mask: u32) {
        self.pending.fetch_or(mask, Ordering::AcqRel);
    }
}

// Only the acquisition task owns deadlines. Events need no per-state timer or
// queue; their mask points directly at native surface positions within this boot.
pub(super) struct Schedule {
    periods: [u32; COUNT],
    deadlines: [u32; COUNT],
    states: u32,
}
impl Schedule {
    pub fn new(native: &'static dyn Native, signals: &'static Signals) -> Self {
        let mut s = Self {
            periods: [0; COUNT],
            deadlines: [0; COUNT],
            states: 0,
        };
        for i in 0..COUNT {
            let Some(source) = native.acquisition_source(i) else {
                break;
            };
            let bit = 1u32 << i;
            s.states |= bit;
            match source {
                Acquisition::Event(event) => event.bind(signals, bit),
                Acquisition::Periodic(interval) => {
                    assert!(
                        interval.get() <= i32::MAX as u32,
                        "acquisition interval exceeds wrapping clock bound"
                    );
                    s.periods[i] = interval.get();
                }
                Acquisition::OnInvocation => {}
            }
        }
        signals.retain(s.states); // every source establishes exactly one baseline
        s
    }
    pub fn due(&self, now: u32, signals: &Signals) -> u32 {
        let mut mask = signals.take();
        for i in 0..COUNT {
            if self.periods[i] != 0 && now.wrapping_sub(self.deadlines[i]) < 1 << 31 {
                mask |= 1 << i;
            }
        }
        mask & self.states
    }
    pub fn completed(&mut self, mask: u32, now: u32) {
        for i in 0..COUNT {
            if mask & (1 << i) != 0 && self.periods[i] != 0 {
                self.deadlines[i] = now.wrapping_add(self.periods[i]);
            }
        }
    }
    pub fn wait_ms(&self, now: u32, signals: &Signals) -> Option<u32> {
        if signals.pending() {
            return Some(0);
        }
        self.periods
            .iter()
            .zip(self.deadlines)
            .filter(|(period, _)| **period != 0)
            .map(|(_, due)| {
                if due.wrapping_sub(now) < 1 << 31 {
                    due.wrapping_sub(now)
                } else {
                    0
                }
            })
            .min()
    }
}
fn now_ms() -> u32 {
    ((tasks::xTaskGetTickCount() as u64 * 1000) / crate::configTICK_RATE_HZ as u64) as u32
}
pub(super) fn run(native: &'static dyn Native, context: &TaskContext) -> ! {
    let signals = native.acquisition_signals().expect("acquisition owner");
    let old = signals
        .task
        .swap(tasks::xTaskGetCurrentTaskHandle(), Ordering::AcqRel);
    assert!(old.is_null(), "acquisition task already installed");
    let mut schedule = Schedule::new(native, signals);
    loop {
        let mask = schedule.due(now_ms(), signals);
        let timeout = if mask != 0 && !native.acquire_selected(context, mask) {
            // Preserve readiness if flash admission or acquisition/cache ownership
            // is temporarily busy. This is contention retry, not idle sampling.
            signals.retain(mask);
            crate::milliseconds_to_ticks_checked(1).unwrap()
        } else {
            let now = now_ms();
            schedule.completed(mask, now);
            schedule.wait_ms(now, signals).map_or(portMAX_DELAY, |ms| {
                crate::milliseconds_to_ticks_checked(ms as u64).unwrap()
            })
        };
        // FreeRTOS retains notifications arriving between pending-mask inspection
        // and blocking. Events during a getter remain pending for the next pass.
        unsafe {
            tasks::xTaskNotifyWait(0, u32::MAX, ptr::null_mut(), timeout);
        }
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn event_and_interval_bounds() {
        let event = std::boxed::Box::leak(std::boxed::Box::new(AcquisitionEvent::new()));
        let owner = std::boxed::Box::leak(std::boxed::Box::new(Signals::new()));
        event.notify(); // before binding: startup baseline will establish current state
        assert_eq!(owner.take(), 0);
        event.bind(owner, 1);
        event.bind(owner, 1 << 31); // shared group includes both ends of the bitmap
        event.notify();
        event.waker().clone().wake();
        event.waker().wake_by_ref();
        assert_eq!(owner.take(), 0x8000_0001);
        assert_eq!(owner.take(), 0);
        event.notify();
        assert_eq!(owner.take(), 0x8000_0001);
        let other = std::boxed::Box::leak(std::boxed::Box::new(Signals::new()));
        assert!(std::panic::catch_unwind(|| event.bind(other, 2)).is_err());
        event.notify();
        assert_eq!(owner.take(), 0x8000_0001);
        assert_eq!(other.take(), 0);
        for ms in [0, 1, i32::MAX as u32, i32::MAX as u32 + 1, u32::MAX] {
            assert_eq!(
                std::panic::catch_unwind(|| Acquisition::periodic(ms)).is_ok(),
                ms != 0 && ms <= i32::MAX as u32
            );
        }
    }
}
