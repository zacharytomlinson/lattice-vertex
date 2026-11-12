//! Current appliance state and qualified hardware events.
//! Registration is kernel bookkeeping; firmware declares sources, not bind calls.
#[cfg(feature = "use-mutexes")]
use super::appliance::changes::{Captured, Events};
use crate::{
    ir::{Type, Value},
    kernel::tasks,
    port,
    surface::Scalar,
    types::*,
};
use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

/// Hardware transition qualification; no remembered input level is needed.
#[derive(Clone, Copy)]
pub enum Edge {
    Rising,
    Falling,
    Either,
}
/// Passive electrical bias for a board input.
#[derive(Clone, Copy)]
pub enum Pull {
    None,
    Up,
    Down,
}
/// A kernel-owned hardware event. The port retains the pin until reset.
/// Enabling/disabling changes the hardware interrupt mask, not a polled flag.
#[derive(Clone, Copy)]
pub struct Interrupt {
    source: u8,
    set: fn(u8, bool),
    read: fn(u8) -> bool,
}
impl Interrupt {
    pub(crate) const fn new(source: u8, set: fn(u8, bool), read: fn(u8) -> bool) -> Self {
        Self { source, set, read }
    }
    /// Read the physical input level; this does not retain or compare old input.
    /// Useful when one either-edge action needs to distinguish ON from OFF.
    pub fn level(&self) -> bool {
        (self.read)(self.source)
    }
    pub fn enable(&self) {
        (self.set)(self.source, true);
    }
    pub fn disable(&self) {
        (self.set)(self.source, false);
    }
}

/// A transition could not be established or retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateChangeError {
    /// A hardware event arrived before kernel initialization connected its source.
    NotReady,
    /// A nonblocking hardware capture exceeded its storage bound. State mutations
    /// do not return this as a rejected operation: they fault after committing.
    Capacity,
    /// A relative operation needs a currently known value.
    Unavailable,
    /// A value cannot be represented by the mapped register field.
    OutOfRange,
}

/// A contiguous field in an ordinary readable 32-bit peripheral register.
/// The record owns access authority, not a cached copy of the register value.
/// Board ports should construct it from their owned HAL peripheral tokens.
pub struct RegisterField {
    read: *const u32,
    write: *mut u32,
    clear: *mut u32,
    mask: u32,
}
unsafe impl Send for RegisterField {}
unsafe impl Sync for RegisterField {}
impl RegisterField {
    /// Map a read/write field. Writes preserve bits outside `mask`.
    ///
    /// # Safety
    /// The address must remain valid and word-aligned for the state's lifetime.
    /// Reads must be side-effect free; ordinary read/modify/write must be valid.
    /// The caller transfers exclusive software ownership of the selected bits;
    /// no DMA/peripheral may modify those bits asynchronously. Unrelated bits
    /// must not be modified concurrently outside the kernel's atomic boundary.
    pub const unsafe fn new(address: *mut u32, mask: u32) -> Self {
        Self::validate(mask);
        Self {
            read: address,
            write: address,
            clear: core::ptr::null_mut(),
            mask,
        }
    }
    /// Map a readable field with write-one-to-set and write-one-to-clear aliases.
    ///
    /// # Safety
    /// All addresses must remain valid and word-aligned. Reads must have no side
    /// effects and reflect the output latch. The alias semantics must hold, and
    /// the caller transfers exclusive software ownership of the selected bits.
    pub const unsafe fn set_clear(
        read: *const u32,
        set: *mut u32,
        clear: *mut u32,
        mask: u32,
    ) -> Self {
        Self::validate(mask);
        Self {
            read,
            write: set,
            clear,
            mask,
        }
    }
    const fn validate(mask: u32) {
        assert!(mask != 0, "empty register field");
        let bits = mask >> mask.trailing_zeros();
        assert!(
            bits & bits.wrapping_add(1) == 0,
            "noncontiguous register field"
        );
    }
    fn read<T: Scalar>(&self) -> T {
        let bits = (unsafe { core::ptr::read_volatile(self.read) } & self.mask)
            >> self.mask.trailing_zeros();
        let value = match T::TYPE {
            Type::Bool => Value::boolean(bits != 0),
            Type::Integer(kind) => {
                let width = self.mask.count_ones();
                let value = if kind.signed() {
                    ((i64::from(bits) << (64 - width)) >> (64 - width)) as i128
                } else {
                    i128::from(bits)
                };
                Value::integer(kind, value).expect("register scalar domain")
            }
        };
        T::from_value(value).expect("register scalar type")
    }
    fn write<T: Scalar>(&self, value: T) -> Result<(), StateChangeError> {
        let width = self.mask.count_ones();
        let bits = match T::TYPE {
            Type::Bool => u32::from(value.into_value().as_bool().unwrap()),
            Type::Integer(kind) => {
                let value = value.into_value().as_integer().unwrap();
                let (min, max) = if kind.signed() {
                    (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
                } else {
                    (0, (1i128 << width) - 1)
                };
                if value < min || value > max {
                    return Err(StateChangeError::OutOfRange);
                }
                value as u32
            }
        };
        let bits = (bits << self.mask.trailing_zeros()) & self.mask;
        unsafe {
            if self.clear.is_null() {
                let old = core::ptr::read_volatile(self.read);
                core::ptr::write_volatile(self.write, (old & !self.mask) | bits);
            } else {
                let low = self.mask & !bits;
                if low != 0 {
                    core::ptr::write_volatile(self.clear, low);
                }
                if bits != 0 {
                    core::ptr::write_volatile(self.write, bits);
                }
            }
        }
        Ok(())
    }
}

enum Storage<T: Scalar> {
    Memory(UnsafeCell<Option<T>>),
    Register(RegisterField),
}
impl<T: Scalar> Storage<T> {
    fn read(&self) -> Option<T> {
        match self {
            Self::Memory(value) => unsafe { *value.get() },
            Self::Register(field) => Some(field.read()),
        }
    }
    fn write(&self, value: Option<T>) -> Result<(), StateChangeError> {
        match self {
            Self::Memory(memory) => {
                unsafe { *memory.get() = value };
                Ok(())
            }
            Self::Register(field) => field.write(value.ok_or(StateChangeError::Unavailable)?),
        }
    }
}

#[cfg(feature = "use-mutexes")]
#[derive(Clone, Copy)]
struct Target {
    events: &'static Events,
    index: u16,
    persistent: bool,
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
    pub(crate) fn connect(&self, events: &'static Events, index: u16, persistent: bool) {
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
            *self.target.get() = Some(Target {
                events,
                index,
                persistent,
            });
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
            if target.persistent {
                super::storage::settings_changed();
            }
            if target.index == u16::MAX {
                return Ok(());
            }
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

/// An authoritative typed value in RAM or a mapped peripheral register.
/// Declare its field once using `StateHandle::new`. The kernel connects it during
/// appliance initialization. There is no separate observation mirror or bind call.
pub struct State<T: Scalar> {
    storage: Storage<T>,
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
            storage: Storage::Memory(UnsafeCell::new(value)),
            registration: Registration::new(),
            locked: AtomicBool::new(false),
        }
    }
    /// Use the mapped register as the sole value source; construction does not
    /// read or write the hardware. Its field width must fit the declared scalar.
    pub const fn mapped(field: RegisterField) -> Self {
        let width = field.mask.count_ones();
        match T::TYPE {
            Type::Bool => assert!(width == 1, "Boolean register field width"),
            Type::Integer(kind) => {
                assert!(width <= kind.width(), "register field exceeds scalar width")
            }
        }
        Self {
            storage: Storage::Register(field),
            registration: Registration::new(),
            locked: AtomicBool::new(false),
        }
    }
    fn access<R>(&self, f: impl FnOnce() -> R) -> R {
        atomic(|| {
            assert!(
                self.locked
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok(),
                "state access reentered its atomic transition"
            );
            struct Unlock<'a>(&'a AtomicBool);
            impl Drop for Unlock<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _guard = Unlock(&self.locked);
            f()
        })
    }
    /// Read current state. None is unknown/unavailable, never a fabricated zero.
    pub fn get(&self) -> Option<T> {
        self.access(|| self.storage.read())
    }
    /// Set known state; true means the value changed.
    pub fn set(&self, value: T) -> Result<bool, StateChangeError> {
        self.replace(Some(value))
    }
    /// Clear the value to its typed zero/false representation.
    pub fn clear(&self) -> Result<bool, StateChangeError> {
        let zero = match T::TYPE {
            Type::Bool => Value::boolean(false),
            Type::Integer(kind) => Value::integer(kind, 0).unwrap(),
        };
        self.set(T::from_value(zero).unwrap())
    }
    /// Register loss of availability.
    pub fn unavailable(&self) -> Result<bool, StateChangeError> {
        self.replace(None)
    }
    /// Replace a known or unavailable value. An identical assignment emits nothing.
    pub fn replace(&self, value: Option<T>) -> Result<bool, StateChangeError> {
        self.update(|_| value)
    }
    /// Compute and commit a transition atomically, then capture its consequence.
    /// The closure must be bounded/nonblocking and must not reenter this state.
    /// Publication overflow is a kernel fault after mutation, never an invitation
    /// to retry a hardware operation. No task or semaphore wait is performed.
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
        self.access(|| {
            let current = self.storage.read();
            let next = update(current)?;
            if current == next {
                return Ok(false);
            }
            self.storage.write(next)?;
            if self.registration.connected() {
                self.registration
                    .change(next.map(Scalar::into_value))
                    .expect("state mutation committed but capture capacity exhausted");
            }
            Ok(true)
        })
    }
}
impl State<bool> {
    /// Toggle known Boolean state and return its new value. Unknown state cannot
    /// be toggled by guessing. Capture failure does not undo the mutation.
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
    restore: Option<unsafe fn(*const (), &A, Option<Value>) -> Result<(), StateChangeError>>,
    event_context: *const (),
    event: unsafe fn(*const (), &A) -> &Registration,
}
unsafe impl<A> Sync for Access<A> {}
impl<A> Access<A> {
    pub const fn memory<T: Scalar>(field: fn(&A) -> &State<T>) -> Self {
        Self {
            read_context: field as *const (),
            read: read_memory::<A, T>,
            restore: Some(restore_memory::<A, T>),
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
            restore: None,
            event_context: event as *const (),
            event: hardware_registration::<A, T>,
        }
    }
    pub fn read(&self, appliance: &A) -> Option<Value> {
        unsafe { (self.read)(self.read_context, appliance) }
    }
    pub const fn writable(&self) -> bool {
        self.restore.is_some()
    }
    pub fn restore(&self, appliance: &A, value: Option<Value>) -> Result<(), StateChangeError> {
        let restore = self.restore.ok_or(StateChangeError::Unavailable)?;
        atomic(|| unsafe { restore(self.read_context, appliance, value) })
    }
    #[cfg(feature = "use-mutexes")]
    pub fn connect(&self, appliance: &A, events: &'static Events, index: u16, persistent: bool) {
        unsafe { (self.event)(self.event_context, appliance) }.connect(events, index, persistent);
    }
}
unsafe fn read_memory<A, T: Scalar>(field: *const (), appliance: &A) -> Option<Value> {
    let field: fn(&A) -> &State<T> = unsafe { core::mem::transmute(field) };
    field(appliance).get().map(Scalar::into_value)
}
unsafe fn restore_memory<A, T: Scalar>(
    field: *const (),
    appliance: &A,
    value: Option<Value>,
) -> Result<(), StateChangeError> {
    let field: fn(&A) -> &State<T> = unsafe { core::mem::transmute(field) };
    let value = match value {
        Some(value) => Some(T::from_value(value).ok_or(StateChangeError::OutOfRange)?),
        None => None,
    };
    field(appliance).storage.write(value)
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

#[cfg(all(test, feature = "port-test"))]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn register_domains() {
        crate::port::test_port_reset();
        let mut word = 0xa500_00a5;
        let address = core::ptr::addr_of_mut!(word);
        // The backing word lives for the complete test and all accesses are
        // sequential. Inspect through the raw pointer, not aliased Rust references.
        {
            let state = State::<u8>::mapped(unsafe { RegisterField::new(address, 0xff00) });
            for value in [0, 1, 127, 255] {
                state.set(value).unwrap();
                assert_eq!(state.get(), Some(value));
                assert_eq!(
                    unsafe { address.read() },
                    0xa500_00a5 | u32::from(value) << 8
                );
            }
            state.clear().unwrap();
            assert_eq!(unsafe { address.read() }, 0xa500_00a5);
            assert_eq!(state.unavailable(), Err(StateChangeError::Unavailable));
        }
        {
            let signed = State::<i16>::mapped(unsafe { RegisterField::new(address, 0xff00) });
            for (value, accepted) in [
                (-129, false),
                (-128, true),
                (-1, true),
                (0, true),
                (127, true),
                (128, false),
            ] {
                let before = unsafe { address.read() };
                let result = signed.set(value);
                assert_eq!(result.is_ok(), accepted);
                if accepted {
                    assert_eq!(signed.get(), Some(value));
                } else {
                    assert_eq!(result, Err(StateChangeError::OutOfRange));
                    assert_eq!(unsafe { address.read() }, before);
                }
            }
        }
        let bit = State::<bool>::mapped(unsafe { RegisterField::new(address, 1 << 20) });
        for expected in [true, false, true] {
            crate::port::test_port_set_inside_interrupt(true);
            assert_eq!(bit.toggle(), Ok(expected));
            crate::port::test_port_set_inside_interrupt(false);
            assert_eq!(bit.get(), Some(expected));
        }
    }

    #[test]
    fn field_validation() {
        for (mask, valid) in [(0, false), (5, false), (0xff00, true), (u32::MAX, true)] {
            let mut word = 0;
            assert_eq!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                    RegisterField::new(core::ptr::addr_of_mut!(word), mask)
                }))
                .is_ok(),
                valid
            );
        }
    }
}
