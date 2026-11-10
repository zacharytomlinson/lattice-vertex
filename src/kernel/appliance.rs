//! Appliance bindings, captured state changes and native dispatch.

pub(crate) mod changes;
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
    sync::atomic::{AtomicPtr, AtomicU8, Ordering},
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
    /// Two exposed states have the same durable binding reference.
    DuplicateBinding,
}

/// Static storage for an appliance's native state and mathematical surface.
/// Initialize once before scheduler startup, then pass it to `kernel::start`.
/// The kernel binds typed change inputs and invokes native functions in its tasks.
/// Firmware owns device behavior; integration requires no report loop.
pub struct Appliance<A: 'static, E: 'static> {
    name: Option<&'static str>,
    surface: &'static Surface<'static, A, E>,
    control: UnsafeCell<StaticQueue_t>,
    native: UnsafeCell<MaybeUninit<A>>,
    maintenance: UnsafeCell<MaybeUninit<Mutex<()>>>,
    instance: AtomicPtr<()>,
    ready: AtomicU8,
    events: changes::Events,
    installed: AtomicPtr<super::history::Installation<A>>,
}

// Initialization is once-only and published with release/acquire. After that,
// A is shared only after publication and must be Sync. The maintenance mutex
// protects derived values, never the lifetime of a native operation.
unsafe impl<A: Send + Sync, E> Sync for Appliance<A, E> {}

impl<A: Send + Sync, E> Appliance<A, E> {
    /// Reserve native storage without an allocator or caller-supplied mutex.
    pub const fn new(surface: &'static Surface<'static, A, E>) -> Self {
        assert!(surface.unique_bindings(), "duplicate appliance binding");
        assert!(
            surface.states.len() <= crate::configLATTICE_NATIVE_STATES,
            "native state capacity"
        );
        Self {
            name: None,
            surface,
            control: UnsafeCell::new(StaticQueue_t::new()),
            native: UnsafeCell::new(MaybeUninit::uninit()),
            maintenance: UnsafeCell::new(MaybeUninit::uninit()),
            instance: AtomicPtr::new(ptr::null_mut()),
            ready: AtomicU8::new(0),
            events: changes::Events::new(),
            installed: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Attach an appliance name in the ordinary static declaration.
    /// The name is separate from durable state/operation bindings and the
    /// encoded surface identity. No registration or special linker section is used.
    ///
    /// # Panics
    /// Rejects an empty name or an embedded NUL, including during const evaluation.
    pub const fn with_name(mut self, name: &'static str) -> Self {
        assert!(!name.is_empty(), "empty appliance name");
        let bytes = name.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            assert!(bytes[index] != 0, "NUL in appliance name");
            index += 1;
        }
        self.name = Some(name);
        self
    }

    /// The optional name supplied by the firmware declaration.
    pub const fn name(&self) -> Option<&'static str> {
        self.name
    }

    /// Install the actual device once, before starting the scheduler.
    ///
    /// Panics on repeated initialization or unavailable static mutex storage.
    pub fn initialize(&'static self, context: &TaskContext, value: A) -> &'static A {
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
        // escape. The distinct mutex serializes derived-state maintenance, not appliance calls.
        unsafe {
            (*self.native.get()).write(value);
            self.instance.store(
                (*self.native.get()).as_ptr().cast_mut().cast(),
                Ordering::Release,
            );
            let maintenance = Mutex::new_static(context, (), &mut *self.control.get())
                .expect("state maintenance mutex");
            (*self.maintenance.get()).write(maintenance);
        }
        self.ready.store(2, Ordering::Release);
        self.events.initialize(context);
        for (index, state) in self.surface.states.iter().enumerate() {
            state.bind(self.native(), &self.events, index as u16);
        }
        self.native()
    }

    fn native(&self) -> &A {
        assert!(
            self.ready.load(Ordering::Acquire) & 2 != 0,
            "initialize appliance before use"
        );
        unsafe { (*self.native.get()).assume_init_ref() }
    }
    /// Access the initialized firmware state machine for long-lived appliance
    /// tasks and event handlers. This borrows no mutex or update hold. Protect
    /// shared physical I/O with its normal kernel/HAL synchronization.
    pub fn device(&'static self) -> &'static A {
        self.native()
    }

    /// Invoke a typed appliance operation locally through the same kernel-owned
    /// update guard used for remote operations. Changes originate in the state/
    /// event primitives used by the operation, not from sampling around this call.
    pub fn call<R>(
        &self,
        context: &TaskContext,
        operation: impl FnOnce(&A) -> Result<R, E>,
    ) -> Result<R, InvokeError<E>> {
        self.operation(context, |device| {
            operation(device).map_err(InvokeError::Appliance)
        })
    }

    fn operation<R>(
        &self,
        context: &TaskContext,
        run: impl FnOnce(&A) -> Result<R, InvokeError<E>>,
    ) -> Result<R, InvokeError<E>> {
        crate::sync::assert_task_context("appliance operation");
        #[cfg(feature = "firmware-update")]
        let _hold = super::update::hold_updates(context).map_err(|_| InvokeError::Busy)?;
        let _ = context;
        run(self.native())
    }

    fn maintenance(&self) -> &Mutex<()> {
        unsafe { (*self.maintenance.get()).assume_init_ref() }
    }

    /// Run a finite operation on the firmware state machine. Native functions use shared access;
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

    fn process_changes(&self, context: &TaskContext) {
        // One consumer handles captured values in FIFO order. The mutex only
        // protects derived-state maintenance/read consistency, never hardware I/O.
        while self.events.pending(context) {
            let guard = self.maintenance().try_lock(context).or_else(|| {
                (tasks::xTaskGetSchedulerState() == tasks::taskSCHEDULER_RUNNING)
                    .then(|| self.maintenance().lock(context))
                    .flatten()
            });
            let Some(guard) = guard else {
                return;
            };
            let Some(change) = self.events.take(context) else {
                break;
            };
            let state = self.surface.states[change.index as usize];
            let batch = super::change::batch();
            state.record_change(self.native(), change.value());
            super::history::maintain(state, self.native(), change.value());
            // Publication may immediately wake the higher-priority peer. Release
            // maintenance first so that wake cannot strand a Busy evaluation.
            drop(guard);
            drop(batch);
        }
    }
    /// Number of source changes rejected by bounded local event storage.
    pub fn state_change_overflow(&self) -> u32 {
        self.events.overflow()
    }

    pub(crate) fn ir_status(&self) -> lattice_peer_wire::IrStatus {
        let status = self.surface.ir_status();
        if status == lattice_peer_wire::IrStatus::Absent && self.installations().next().is_some() {
            lattice_peer_wire::IrStatus::Incomplete
        } else {
            status
        }
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
        state: &'static super::history::RetainedState<A, T, U, N>,
        update: fn(&super::history::History<T, N>) -> Option<U>,
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
            for (index, candidate) in state.states().enumerate() {
                let reference = candidate.location.reference();
                if self
                    .state_definitions()
                    .any(|s| s.location.reference() == reference && !ptr::eq(s, candidate))
                    || state
                        .states()
                        .take(index)
                        .any(|s| s.location.reference() == reference)
                {
                    return Err(InstallError::DuplicateBinding);
                }
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
        let Some(_guard) = self.maintenance().try_lock(context) else {
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
    ) -> impl Iterator<Item = &'static super::history::Installation<A>> + Clone {
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
        self.surface
            .transformers
            .iter()
            .filter_map(|t| t.definition())
    }
    fn axioms(&self) -> impl Iterator<Item = &crate::ir::Predicate<'_, '_, Self::Location>> {
        self.surface
            .axioms()
            .iter()
            .copied()
            .chain(self.installations().map(|i| i.definition))
    }
    fn invariants(&self) -> impl Iterator<Item = &crate::ir::Predicate<'_, '_, Self::Location>> {
        self.surface.invariants().iter().copied()
    }
}

pub(crate) trait Native: Sync {
    fn ready(&self) -> bool;
    fn seal(&self) {}
    fn process_changes(&self, _context: &TaskContext) {}
    fn wait_changes(&self, _context: &TaskContext) {}
    fn owns(&self, instance: *const (), attribute: u64) -> bool;
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult;
    fn invoke(&self, context: &TaskContext, transformer: u64, arguments: &[Value]) -> InvokeStatus;
    fn state_binding(
        &self,
        _reference: u64,
    ) -> Option<(crate::ir::Type, &core::sync::atomic::AtomicU16)> {
        None
    }
    fn transformer_signature(&self, _reference: u64) -> Option<&[crate::ir::Type]> {
        None
    }
    fn clear_dependencies(&self) {}
}

impl<A: Send + Sync, E> Native for Appliance<A, E> {
    fn state_binding(
        &self,
        reference: u64,
    ) -> Option<(crate::ir::Type, &core::sync::atomic::AtomicU16)> {
        self.state_definitions()
            .find(|s| s.location.reference() == reference)
            .map(|s| (s.ty, &s.location.dependencies))
    }
    fn transformer_signature(&self, reference: u64) -> Option<&[crate::ir::Type]> {
        self.surface
            .transformers
            .iter()
            .find(|t| t.reference() == reference)
            .map(|t| t.arguments())
    }
    fn clear_dependencies(&self) {
        for state in self.state_definitions() {
            state
                .location
                .dependencies
                .store(u16::MAX, Ordering::Release);
        }
    }
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
                .any(|s| s.location.reference() == attribute)
    }
    fn process_changes(&self, context: &TaskContext) {
        Appliance::process_changes(self, context);
    }
    fn wait_changes(&self, context: &TaskContext) {
        self.events.wait(context);
    }
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult {
        if !self.ready() {
            return StateReadResult::Busy;
        }
        let Some(_guard) = self.maintenance().try_lock(context) else {
            return StateReadResult::Busy;
        };
        super::state::atomic(|| {
            // Never return a new primitive value with a pre-transition sequence.
            // The event consumer publishes every queued transition first.
            if self.events.pending(context) {
                return StateReadResult::Busy;
            }
            let value = if let Some(state) = self
                .surface
                .states
                .iter()
                .find(|s| s.reference() == attribute)
            {
                state.read(self.native())
            } else if let Some(state) = self
                .installations()
                .flat_map(|i| i.states())
                .find(|s| s.location.reference() == attribute)
            {
                state.location.read(self.native())
            } else {
                return StateReadResult::UnknownAttribute;
            };
            StateReadResult::Value {
                sequence: super::change::sequence(),
                value,
            }
        })
    }

    fn invoke(&self, context: &TaskContext, transformer: u64, arguments: &[Value]) -> InvokeStatus {
        let Some(handle) = self
            .surface
            .transformers
            .iter()
            .find(|h| h.reference() == transformer)
        else {
            return InvokeStatus::UnknownTransformer;
        };
        let result = match self.operation(context, |a| handle.invoke(context, a, arguments)) {
            Ok(()) => InvokeStatus::Invoked,
            Err(InvokeError::Busy) => InvokeStatus::Busy,
            Err(InvokeError::Arguments(_)) => InvokeStatus::Arguments,
            Err(InvokeError::Appliance(_)) => InvokeStatus::ApplianceError,
        };
        result
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests;

/// Construct a named appliance in a static declaration.
///
/// The name may be a literal, a constant, or an expression such as `env!(...)`.
/// Expansion uses the ordinary const constructor, without runtime registration.
///
/// ```
/// use lattice_vertex::{appliance, kernel::Appliance, surface};
/// use core::convert::Infallible;
///
/// static DEVICE: Appliance<(), Infallible> = appliance! {
///     name: concat!("Kitchen", " light"),
///     surface: &surface::EMPTY,
/// };
/// assert_eq!(DEVICE.name(), Some("Kitchen light"));
/// ```
///
/// ```compile_fail
/// use lattice_vertex::{appliance, kernel::Appliance, surface};
/// use core::convert::Infallible;
/// static INVALID: Appliance<(), Infallible> = appliance! {
///     name: "",
///     surface: &surface::EMPTY,
/// };
/// ```
#[macro_export]
macro_rules! appliance {
    (name: $name:expr, surface: $surface:expr $(,)?) => {
        $crate::kernel::Appliance::new($surface).with_name($name)
    };
}
