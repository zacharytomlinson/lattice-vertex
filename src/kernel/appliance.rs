//! Resident appliance storage and synchronized native dispatch.

use super::{
    peer::frame::{InvokeStatus, StateReadResult},
    queue::StaticQueue_t,
    tasks,
};
use crate::{
    ir::Value,
    surface::{InvokeError, Surface},
    sync::{Mutex, TaskContext},
};
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU8, Ordering},
};

/// Static storage for an appliance's native state and mathematical surface.
/// Initialize once before scheduler startup, then pass it to `kernel::start`.
/// Hardware tasks use `with` to apply established observations through the
/// existing `StateHandle::update` / `observation::update` interfaces.
pub struct Appliance<A: 'static, E: 'static> {
    surface: &'static Surface<'static, A, E>,
    control: UnsafeCell<StaticQueue_t>,
    native: UnsafeCell<MaybeUninit<Mutex<A>>>,
    instance: AtomicPtr<()>,
    ready: AtomicU8,
}

// Initialization is once-only and published with release/acquire. After that,
// the existing RTOS mutex exclusively protects A. The surface is immutable.
unsafe impl<A: Send, E> Sync for Appliance<A, E> {}

impl<A: Send, E> Appliance<A, E> {
    /// Reserve native storage without an allocator or caller-supplied mutex.
    pub const fn new(surface: &'static Surface<'static, A, E>) -> Self {
        Self {
            surface,
            control: UnsafeCell::new(StaticQueue_t::new()),
            native: UnsafeCell::new(MaybeUninit::uninit()),
            instance: AtomicPtr::new(ptr::null_mut()),
            ready: AtomicU8::new(0),
        }
    }

    /// Install the actual device once, before starting the scheduler.
    ///
    /// Panics on repeated initialization or unavailable static mutex storage.
    pub fn initialize(&'static self, context: &TaskContext, value: A) {
        assert_eq!(
            tasks::xTaskGetSchedulerState(),
            tasks::taskSCHEDULER_NOT_STARTED
        );
        assert!(
            self.ready
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "appliance already initialized"
        );
        // The static control block is exclusively initialized once. Neither it
        // nor the inline mutex/native value can move after this publication.
        unsafe {
            let mutex = Mutex::new_static(context, value, &mut *self.control.get())
                .expect("appliance mutex");
            (*self.native.get()).write(mutex);
            let guard = (*self.native.get())
                .assume_init_ref()
                .try_lock(context)
                .unwrap();
            self.instance
                .store(ptr::from_ref(&*guard).cast_mut().cast(), Ordering::Release);
        }
        self.ready.store(2, Ordering::Release);
    }

    fn native(&self) -> &Mutex<A> {
        assert_eq!(
            self.ready.load(Ordering::Acquire),
            2,
            "initialize appliance before use"
        );
        unsafe { (*self.native.get()).assume_init_ref() }
    }

    /// Access native state in task context. Register physical observations with
    /// the state update primitives inside this closure; their networking is
    /// automatic once the kernel binds this instance. The closure must not
    /// recursively acquire this same appliance.
    pub fn with<R>(&self, context: &TaskContext, update: impl FnOnce(&mut A) -> R) -> R {
        #[cfg(feature = "firmware-update")]
        let _activity = loop {
            if let Ok(guard) = super::update::hold_updates(context) {
                break guard;
            }
            // A flash step or activation owns admission. Start hardware work
            // only after it completes (or the selected image resets).
            crate::delay_ms(context, 1);
        };
        let mutex = self.native();
        let mut guard = if tasks::xTaskGetSchedulerState() == tasks::taskSCHEDULER_NOT_STARTED {
            mutex.try_lock(context)
        } else {
            mutex.lock(context)
        }
        .expect("appliance lock");
        update(&mut guard)
    }

    pub(crate) fn surface(&self) -> &'static Surface<'static, A, E> {
        self.surface
    }
}

pub(crate) trait Native: Sync {
    fn ready(&self) -> bool;
    fn owns(&self, instance: *const (), attribute: u64) -> bool;
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult;
    fn invoke(&self, context: &TaskContext, transformer: u64, arguments: &[Value]) -> InvokeStatus;
}

impl<A: Send, E> Native for Appliance<A, E> {
    fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire) == 2
    }
    fn owns(&self, instance: *const (), attribute: u64) -> bool {
        self.instance.load(Ordering::Acquire).cast_const() == instance
            && self
                .surface
                .states
                .iter()
                .any(|s| ptr::from_ref(s.definition()) as usize as u64 == attribute)
    }
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult {
        let Some(state) = self
            .surface
            .states
            .iter()
            .find(|s| ptr::from_ref(s.definition()) as usize as u64 == attribute)
        else {
            return StateReadResult::UnknownAttribute;
        };
        let Some(guard) = self.native().try_lock(context) else {
            return StateReadResult::Busy;
        };
        // Hold the same mutex used for observation updates while reading the
        // value and its change watermark. A read neither registers nor drains deltas.
        let value = state.read(&guard);
        let sequence = super::change::sequence();
        StateReadResult::Value { sequence, value }
    }

    fn invoke(&self, context: &TaskContext, transformer: u64, arguments: &[Value]) -> InvokeStatus {
        #[cfg(feature = "firmware-update")]
        let Ok(_activity) = super::update::hold_updates(context) else {
            return InvokeStatus::Busy;
        };
        let Some(handle) = self
            .surface
            .transformers
            .iter()
            .find(|h| ptr::from_ref(**h) as usize as u64 == transformer)
        else {
            return InvokeStatus::UnknownTransformer;
        };
        let Some(mut guard) = self.native().try_lock(context) else {
            return InvokeStatus::Busy;
        };
        match handle.invoke(&mut guard, arguments) {
            Ok(()) => InvokeStatus::Invoked,
            Err(InvokeError::Arguments(_)) => InvokeStatus::Arguments,
            Err(InvokeError::Appliance(_)) => InvokeStatus::ApplianceError,
        }
    }
}
