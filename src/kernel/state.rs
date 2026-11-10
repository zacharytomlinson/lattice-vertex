//! Current appliance state and qualified hardware events.
//! Registration is kernel bookkeeping; firmware declares sources, not bind calls.
#[cfg(feature = "use-mutexes")]
use super::appliance::changes::{Captured, Events};
use crate::{ir::Value, kernel::tasks, port, surface::Scalar, types::*};
use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

/// A transition could not be established or retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateChangeError {
    /// A hardware event arrived before kernel initialization connected its source.
    NotReady,
    /// Captured-event storage is full. Accepted entries remain intact. A rejected
    /// software write leaves its state unchanged; hardware must retain/recover
    /// an event it has already produced or report its loss.
    Capacity,
    /// A relative operation needs a currently known value.
    Unavailable,
}

#[cfg(feature = "use-mutexes")]
#[derive(Clone, Copy)]
struct Target {
    events: &'static Events,
    index: u16,
}
/// Callback context for one state. Never exposed as an appliance concept.
pub(crate) struct Registration {
    #[cfg(feature = "use-mutexes")]
    target: UnsafeCell<Option<Target>>,
    bound: AtomicU8,
}
unsafe impl Sync for Registration {}
impl Registration {
    const fn new() -> Self {
        Self {
            #[cfg(feature = "use-mutexes")]
            target: UnsafeCell::new(None),
            bound: AtomicU8::new(0),
        }
    }
    #[cfg(feature = "use-mutexes")]
    pub(crate) fn connect(&self, events: &'static Events, index: u16) {
        assert_eq!(
            tasks::xTaskGetSchedulerState(),
            tasks::taskSCHEDULER_NOT_STARTED
        );
        assert!(
            self.bound
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok(),
            "state source declared more than once"
        );
        unsafe {
            *self.target.get() = Some(Target { events, index });
        }
        self.bound.store(2, Ordering::Release);
    }
    fn connected(&self) -> bool {
        self.bound.load(Ordering::Acquire) == 2
    }
    fn change(&self, value: Option<Value>) -> Result<(), StateChangeError> {
        #[cfg(feature = "use-mutexes")]
        if self.connected() {
            let target = unsafe { (*self.target.get()).unwrap() };
            return target.events.send(Captured::new(target.index, value));
        }
        let _ = value;
        Err(StateChangeError::NotReady)
    }
}

/// A resident endpoint for a qualified hardware change. Declare its reader and
/// this event together with `StateHandle::hardware`; initialization connects it.
/// The HAL callback supplies a captured value, never a request to poll a getter.
pub struct StateEvent<T: Scalar> {
    registration: Registration,
    _value: PhantomData<T>,
}
unsafe impl<T: Scalar> Sync for StateEvent<T> {}
impl<T: Scalar> StateEvent<T> {
    /// Declare the event before enabling its HAL callback.
    pub const fn new() -> Self {
        Self {
            registration: Registration::new(),
            _value: PhantomData,
        }
    }
    /// Capture/acknowledge an already-qualified change in a short nonblocking
    /// critical section. None means unavailable. The hook still runs before
    /// initialization so an IRQ can be acknowledged, then NotReady is returned.
    pub fn handle(&self, capture: impl FnOnce() -> Option<T>) -> Result<(), StateChangeError> {
        atomic(|| self.registration.change(capture().map(Scalar::into_value)))
    }
}
impl<T: Scalar> Default for StateEvent<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// An appliance's software-maintained state, with automatic change registration.
/// Declare its field once using `StateHandle::new`. The kernel connects it during
/// appliance initialization. There is no separate observation mirror or bind call.
pub struct State<T: Scalar> {
    value: UnsafeCell<Option<T>>,
    registration: Registration,
    locked: AtomicBool,
}
unsafe impl<T: Scalar + Send> Sync for State<T> {}
impl<T: Scalar> State<T> {
    /// Initialize known state without an actuator call or delta.
    pub const fn new(value: T) -> Self {
        Self::restore(Some(value))
    }
    /// Initialize state whose value is not yet known.
    pub const fn unknown() -> Self {
        Self::restore(None)
    }
    /// Restore a supplied known/unavailable value, without replaying an operation.
    pub const fn restore(value: Option<T>) -> Self {
        Self {
            value: UnsafeCell::new(value),
            registration: Registration::new(),
            locked: AtomicBool::new(false),
        }
    }
    fn access<R>(&self, f: impl FnOnce(&mut Option<T>) -> R) -> R {
        atomic(|| {
            while self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            struct Unlock<'a>(&'a AtomicBool);
            impl Drop for Unlock<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _guard = Unlock(&self.locked);
            f(unsafe { &mut *self.value.get() })
        })
    }
    /// Read current state. None is unknown/unavailable, never a fabricated zero.
    pub fn get(&self) -> Option<T> {
        self.access(|value| *value)
    }
    /// Set known state; true means the value changed.
    pub fn set(&self, value: T) -> Result<bool, StateChangeError> {
        self.replace(Some(value))
    }
    /// Register loss of availability.
    pub fn unavailable(&self) -> Result<bool, StateChangeError> {
        self.replace(None)
    }
    /// Replace a known or unavailable value. An identical assignment emits nothing.
    pub fn replace(&self, value: Option<T>) -> Result<bool, StateChangeError> {
        self.update(|_| value)
    }
    /// Compute a software transition atomically. The closure must be short and
    /// nonblocking, perform no hardware I/O and not reenter this state.
    pub fn update(
        &self,
        update: impl FnOnce(Option<T>) -> Option<T>,
    ) -> Result<bool, StateChangeError> {
        self.change(|current| Ok(update(current)))
    }
    fn change(
        &self,
        update: impl FnOnce(Option<T>) -> Result<Option<T>, StateChangeError>,
    ) -> Result<bool, StateChangeError> {
        self.access(|current| {
            let next = update(*current)?;
            if *current == next {
                return Ok(false);
            }
            if self.registration.connected() {
                self.registration.change(next.map(Scalar::into_value))?;
            }
            *current = next;
            Ok(true)
        })
    }
}
impl State<bool> {
    /// Toggle known Boolean state and return its new value. Unknown state cannot
    /// be toggled by guessing, and capacity failure leaves the old value intact.
    pub fn toggle(&self) -> Result<bool, StateChangeError> {
        let mut next = false;
        self.change(|current| {
            next = !current.ok_or(StateChangeError::Unavailable)?;
            Ok(Some(next))
        })?;
        Ok(next)
    }
}

/// Erased field access. Typed constructors ensure each function pointer is only
/// restored to the exact signature with which it entered the declaration.
/// No data pointer or firmware address is emitted on the wire.
pub(crate) struct Access<A> {
    read_context: *const (),
    read: unsafe fn(*const (), &A) -> Option<Value>,
    event_context: *const (),
    event: unsafe fn(*const (), &A) -> &Registration,
}
unsafe impl<A> Sync for Access<A> {}
impl<A> Access<A> {
    pub const fn memory<T: Scalar>(field: fn(&A) -> &State<T>) -> Self {
        Self {
            read_context: field as *const (),
            read: read_memory::<A, T>,
            event_context: field as *const (),
            event: memory_registration::<A, T>,
        }
    }
    pub const fn hardware<T: Scalar>(
        reader: fn(&A) -> Option<T>,
        event: fn(&A) -> &StateEvent<T>,
    ) -> Self {
        Self {
            read_context: reader as *const (),
            read: read_hardware::<A, T>,
            event_context: event as *const (),
            event: hardware_registration::<A, T>,
        }
    }
    pub fn read(&self, appliance: &A) -> Option<Value> {
        unsafe { (self.read)(self.read_context, appliance) }
    }
    #[cfg(feature = "use-mutexes")]
    pub fn connect(&self, appliance: &A, events: &'static Events, index: u16) {
        unsafe { (self.event)(self.event_context, appliance) }.connect(events, index);
    }
}
unsafe fn read_memory<A, T: Scalar>(field: *const (), appliance: &A) -> Option<Value> {
    let field: fn(&A) -> &State<T> = unsafe { core::mem::transmute(field) };
    field(appliance).get().map(Scalar::into_value)
}
unsafe fn memory_registration<A, T: Scalar>(field: *const (), appliance: &A) -> &Registration {
    let field: fn(&A) -> &State<T> = unsafe { core::mem::transmute(field) };
    &field(appliance).registration
}
unsafe fn read_hardware<A, T: Scalar>(reader: *const (), appliance: &A) -> Option<Value> {
    let reader: fn(&A) -> Option<T> = unsafe { core::mem::transmute(reader) };
    reader(appliance).map(Scalar::into_value)
}
unsafe fn hardware_registration<A, T: Scalar>(field: *const (), appliance: &A) -> &Registration {
    let field: fn(&A) -> &StateEvent<T> = unsafe { core::mem::transmute(field) };
    &field(appliance).registration
}

fn assert_context() {
    if port::xPortIsInsideInterrupt() == pdFALSE {
        return;
    }
    #[cfg(any(
        feature = "port-cortex-m3",
        feature = "port-cortex-m4f",
        feature = "port-cortex-m7",
        feature = "port-nrf52840"
    ))]
    unsafe {
        let exception: u32;
        core::arch::asm!("mrs {}, IPSR", out(reg) exception, options(nomem, nostack, preserves_flags));
        let exception = (exception & 0x1ff) as usize;
        assert!(exception >= 16, "state event requires a peripheral ISR");
        let priority = core::ptr::read_volatile((0xe000_e400usize + exception - 16) as *const u8);
        assert!(
            u32::from(priority) >= crate::configMAX_SYSCALL_INTERRUPT_PRIORITY,
            "state event IRQ priority cannot enter the kernel"
        );
    }
}
pub(crate) fn atomic<R>(f: impl FnOnce() -> R) -> R {
    assert_context();
    let suspend = port::xPortIsInsideInterrupt() == pdFALSE
        && tasks::xTaskGetSchedulerState() != tasks::taskSCHEDULER_NOT_STARTED;
    // A queue can request a synchronous task yield on some ports. Keep its
    // newly-ready consumer from running before the state write and lock release.
    if suspend {
        unsafe {
            tasks::vTaskSuspendAll();
        }
    }
    struct Restore {
        mask: UBaseType_t,
        suspended: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            port::portCLEAR_INTERRUPT_MASK_FROM_ISR(self.mask);
            if self.suspended {
                unsafe {
                    tasks::xTaskResumeAll();
                }
            }
        }
    }
    let _guard = Restore {
        mask: port::portSET_INTERRUPT_MASK_FROM_ISR(),
        suspended: suspend,
    };
    f()
}
