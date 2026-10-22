//! Passive appliance binding, kernel-owned acquisition and native dispatch.

pub(crate) mod execution;

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
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering},
};

/// Local retained-state installation failure. No new binding is published on failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallError {
    /// Initialize the owning appliance first.
    NotInitialized,
    /// Installation or native access is already in progress.
    Busy,
    /// Surface publication/startup closed installation.
    Closed,
    /// The supplied storage has already been bound to an instance.
    AlreadyInstalled,
    /// The source is not an exposed native state of this appliance.
    UnknownSource,
    /// The history's native scalar type disagrees with its source.
    Type,
    /// The supplied mathematical predicate has an invalid input signature.
    Definition,
}

/// Static storage for an appliance's native state and mathematical surface.
/// Initialize once before scheduler startup, then pass it to `kernel::start`.
/// The kernel samples getters and invokes native functions in its own tasks.
/// The adapter has no report loop. Hardware resources use their own scoped access.
pub struct Appliance<A: 'static, E: 'static> {
    surface: &'static Surface<'static, A, E>,
    control: UnsafeCell<StaticQueue_t>,
    native: UnsafeCell<MaybeUninit<A>>,
    observations: UnsafeCell<MaybeUninit<Mutex<Observations>>>,
    instance: AtomicPtr<()>,
    ready: AtomicU8,
    sampled: AtomicBool,
    acquiring: AtomicBool,
    installed: AtomicPtr<super::state::Installation<A>>,
}

// Initialization is once-only and published with release/acquire. After that,
// A is shared only after publication and must be Sync. The observation mutex
// protects acquisition/history, never the lifetime of a native operation.
unsafe impl<A: Send + Sync, E> Sync for Appliance<A, E> {}

impl<A: Send + Sync, E> Appliance<A, E> {
    /// Reserve native storage without an allocator or caller-supplied mutex.
    pub const fn new(surface: &'static Surface<'static, A, E>) -> Self {
        assert!(
            surface.states.len() <= crate::configLATTICE_NATIVE_STATES,
            "native state capacity"
        );
        Self {
            surface,
            control: UnsafeCell::new(StaticQueue_t::new()),
            native: UnsafeCell::new(MaybeUninit::uninit()),
            observations: UnsafeCell::new(MaybeUninit::uninit()),
            instance: AtomicPtr::new(ptr::null_mut()),
            ready: AtomicU8::new(0),
            sampled: AtomicBool::new(false),
            acquiring: AtomicBool::new(false),
            installed: AtomicPtr::new(ptr::null_mut()),
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
        // The initialized native object never moves and only shared references
        // escape. The distinct mutex serializes acquisition, not appliance calls.
        unsafe {
            (*self.native.get()).write(value);
            self.instance.store(
                (*self.native.get()).as_ptr().cast_mut().cast(),
                Ordering::Release,
            );
            let observations =
                Mutex::new_static(context, Observations::new(), &mut *self.control.get())
                    .expect("observation mutex");
            (*self.observations.get()).write(observations);
        }
        self.ready.store(2, Ordering::Release);
    }

    fn native(&self) -> &A {
        assert!(
            self.ready.load(Ordering::Acquire) & 2 != 0,
            "initialize appliance before use"
        );
        unsafe { (*self.native.get()).assume_init_ref() }
    }
    fn observations(&self) -> &Mutex<Observations> {
        unsafe { (*self.observations.get()).assume_init_ref() }
    }

    /// Access the passive hardware adapter. Native functions use shared access;
    /// synchronization of physical resources belongs to their hardware binding.
    pub fn with<R>(&self, context: &TaskContext, access: impl FnOnce(&A) -> R) -> R {
        #[cfg(feature = "firmware-update")]
        let _hold = loop {
            if let Ok(hold) = super::update::hold_updates(context) {
                break hold;
            }
            crate::delay_ms(context, 1);
        };
        let _ = context;
        access(self.native())
    }

    fn sample(&self, context: &TaskContext) {
        let native = self.native();
        #[cfg(feature = "firmware-update")]
        let Ok(_hold) = super::update::hold_observation(context) else {
            return;
        };
        if self
            .acquiring
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        struct Finish<'a>(&'a AtomicBool);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _acquisition = Finish(&self.acquiring);
        let mut after = [None; crate::configLATTICE_NATIVE_STATES];
        for (index, state) in self.surface.states.iter().enumerate() {
            after[index] = state.read(native);
        }
        // No observation mutex is held during physical I/O. Readers can return
        // the previous established observation while a getter is in progress.
        let Some(mut samples) = self.observations().try_lock(context) else {
            return;
        };
        let batch = super::change::batch();
        // Capture every native input before maintenance publishes derived state.
        for (index, state) in self.surface.states.iter().enumerate() {
            if samples.values[index].is_some_and(|before| before != after[index]) {
                state.record_change(native, after[index]);
            }
        }
        for (index, state) in self.surface.states.iter().enumerate() {
            if let Some(before) = samples.values[index] {
                if before != after[index] {
                    super::state::maintain(state, native, after[index]);
                }
            }
            // First acquisition establishes the baseline, not an invented change.
            samples.values[index] = Some(after[index]);
        }
        self.sampled.store(true, Ordering::Release);
        drop(samples);
        drop(batch);
    }

    pub(crate) fn surface(&self) -> &'static Surface<'static, A, E> {
        self.surface
    }

    /// Bind permanently resident history and an already-compiled updater before
    /// kernel startup/publication. The predicate describes the computed result
    /// mathematically; the kernel never interprets it. History starts empty.
    pub fn install_state<
        T: crate::surface::Scalar + Send,
        U: crate::surface::Scalar + Send,
        const N: usize,
    >(
        &'static self,
        context: &TaskContext,
        state: &'static super::state::RetainedState<A, T, U, N>,
        update: fn(&super::state::History<T, N>) -> Option<U>,
        definition: &'static crate::surface::Predicate<A>,
    ) -> Result<(), InstallError> {
        self.with_installation(context, |appliance| {
            if !self
                .surface
                .states
                .iter()
                .any(|s| ptr::eq(*s, state.source()))
            {
                return Err(InstallError::UnknownSource);
            }
            let next = unsafe { self.installed.load(Ordering::Acquire).as_ref() };
            let node = state.install(
                ptr::from_ref(appliance).cast_mut(),
                update,
                definition,
                next,
            )?;
            self.installed
                .store(ptr::from_ref(node).cast_mut(), Ordering::Release);
            Ok(())
        })
    }

    fn with_installation<R>(
        &'static self,
        context: &TaskContext,
        install: impl FnOnce(&A) -> Result<R, InstallError>,
    ) -> Result<R, InstallError> {
        if tasks::xTaskGetSchedulerState() != tasks::taskSCHEDULER_NOT_STARTED {
            return Err(InstallError::Closed);
        }
        self.ready
            .compare_exchange(2, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|s| match s {
                0 => InstallError::NotInitialized,
                3 => InstallError::Closed,
                _ => InstallError::Busy,
            })?;
        struct Finish<'a>(&'a AtomicU8);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                self.0.store(2, Ordering::Release);
            }
        }
        let _finish = Finish(&self.ready);
        let native = unsafe { (*self.native.get()).assume_init_ref() };
        let Some(_guard) = self.observations().try_lock(context) else {
            return Err(InstallError::Busy);
        };
        install(native)
    }

    pub(crate) fn seal(&self) {
        match self
            .ready
            .compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(3) => {}
            _ => panic!("initialize appliance and finish local installation before publication"),
        }
    }
    pub(crate) fn installations(
        &self,
    ) -> impl Iterator<Item = &'static super::state::Installation<A>> + Clone {
        let first = unsafe { self.installed.load(Ordering::Acquire).as_ref() };
        core::iter::successors(first, |node| node.next_appliance)
    }
    fn state_definitions(
        &self,
    ) -> impl Iterator<Item = &'static crate::ir::Attribute<crate::surface::Read<A>>> {
        self.surface
            .states
            .iter()
            .copied()
            .map(crate::surface::StateHandle::definition)
            .chain(self.installations().flat_map(|node| node.states()))
    }
}

impl<A: Send + Sync + 'static, E: 'static> crate::ir::Surface for Appliance<A, E> {
    type Location = crate::surface::Read<A>;
    fn states<'s>(&'s self) -> impl Iterator<Item = &'s crate::ir::Attribute<Self::Location>> {
        self.state_definitions()
            .map(|state| -> &'s crate::ir::Attribute<Self::Location> { state })
    }
    fn transformers(&self) -> impl Iterator<Item = &crate::ir::Operation<'_, '_, Self::Location>> {
        self.surface.transformers.iter().map(|t| t.definition())
    }
    fn axioms(&self) -> impl Iterator<Item = &crate::ir::Predicate<'_, '_, Self::Location>> {
        self.surface
            .axioms
            .iter()
            .copied()
            .chain(self.installations().map(|i| i.definition))
    }
    fn invariants(&self) -> impl Iterator<Item = &crate::ir::Predicate<'_, '_, Self::Location>> {
        self.surface.invariants.iter().copied()
    }
}

pub(crate) trait Native: Sync {
    fn ready(&self) -> bool;
    fn seal(&self) {}
    fn acquire(&self, _context: &TaskContext) {}
    fn owns(&self, instance: *const (), attribute: u64) -> bool;
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult;
    fn invoke(&self, context: &TaskContext, transformer: u64, arguments: &[Value]) -> InvokeStatus;
}

impl<A: Send + Sync, E> Native for Appliance<A, E> {
    fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire) & 2 == 2
    }
    fn seal(&self) {
        Appliance::seal(self);
    }
    fn owns(&self, instance: *const (), attribute: u64) -> bool {
        self.instance.load(Ordering::Acquire).cast_const() == instance
            && self
                .state_definitions()
                .any(|s| ptr::from_ref(s) as usize as u64 == attribute)
    }
    fn acquire(&self, context: &TaskContext) {
        self.sample(context);
    }
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult {
        if !self.ready() {
            return StateReadResult::Busy;
        }
        if let Some(index) = self
            .surface
            .states
            .iter()
            .position(|s| ptr::from_ref(s.definition()) as usize as u64 == attribute)
        {
            let Some(samples) = self.observations().try_lock(context) else {
                return StateReadResult::Busy;
            };
            return StateReadResult::Value {
                sequence: super::change::sequence(),
                value: samples.values[index].flatten(),
            };
        }
        let Some(state) = self
            .installations()
            .flat_map(|i| i.states())
            .find(|s| ptr::from_ref(*s) as usize as u64 == attribute)
        else {
            return StateReadResult::UnknownAttribute;
        };
        let Some(_guard) = self.observations().try_lock(context) else {
            return StateReadResult::Busy;
        };
        StateReadResult::Value {
            sequence: super::change::sequence(),
            value: state.location.read(self.native()),
        }
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
        self.sample(context);
        if !self.sampled.load(Ordering::Acquire) {
            return InvokeStatus::Busy;
        }
        let result = match handle.invoke(self.native(), arguments) {
            Ok(()) => InvokeStatus::Invoked,
            Err(InvokeError::Arguments(_)) => InvokeStatus::Arguments,
            Err(InvokeError::Appliance(_)) => InvokeStatus::ApplianceError,
        };
        self.sample(context);
        result
    }
}

struct Observations {
    values: [Option<Option<Value>>; crate::configLATTICE_NATIVE_STATES],
}
impl Observations {
    const fn new() -> Self {
        Self {
            values: [None; crate::configLATTICE_NATIVE_STATES],
        }
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests;
