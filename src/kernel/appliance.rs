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
/// Hardware tasks use `with` to apply established observations through the
/// existing `StateHandle::update` / `observation::update` interfaces.
pub struct Appliance<A: 'static, E: 'static> {
    surface: &'static Surface<'static, A, E>,
    control: UnsafeCell<StaticQueue_t>,
    native: UnsafeCell<MaybeUninit<Mutex<A>>>,
    instance: AtomicPtr<()>,
    ready: AtomicU8,
    installed: AtomicPtr<super::state::Installation<A>>,
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
            self.ready.load(Ordering::Acquire) & 2,
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
        let mutex = unsafe { (*self.native.get()).assume_init_ref() };
        let Some(guard) = mutex.try_lock(context) else {
            return Err(InstallError::Busy);
        };
        install(&guard)
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

impl<A: Send + 'static, E: 'static> crate::ir::Surface for Appliance<A, E> {
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
    fn owns(&self, instance: *const (), attribute: u64) -> bool;
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult;
    fn invoke(&self, context: &TaskContext, transformer: u64, arguments: &[Value]) -> InvokeStatus;
}

impl<A: Send, E> Native for Appliance<A, E> {
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
    fn read(&self, context: &TaskContext, attribute: u64) -> StateReadResult {
        let Some(state) = self
            .state_definitions()
            .find(|s| ptr::from_ref(*s) as usize as u64 == attribute)
        else {
            return StateReadResult::UnknownAttribute;
        };
        let Some(guard) = self.native().try_lock(context) else {
            return StateReadResult::Busy;
        };
        // Hold the same mutex used for observation updates while reading the
        // value and its change watermark. A read neither registers nor drains deltas.
        let value = state.location.read(&guard);
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

#[cfg(all(test, feature = "port-test"))]
mod tests {
    extern crate std;
    use super::*;
    use crate::{
        ir::{Effect, Expr},
        observation::{self, Dependent},
        surface::{
            Arguments, Definition, Operation, Predicate, State, StateHandle, StateTransformer,
            TransformerHandle,
        },
    };
    type Device = [Option<bool>; 2];
    struct Slot<const I: usize>;
    impl<const I: usize> State for Slot<I> {
        type Appliance = Device;
        type Value = bool;
        fn read(a: &Device) -> Option<bool> {
            a[I]
        }
    }
    static FIRST: StateHandle<Device> = StateHandle::new::<Slot<0>>();
    static SECOND: StateHandle<Device> = StateHandle::new::<Slot<1>>();
    static TRUE: Definition<Device> = Definition {
        body: Expr::Constant(Value::boolean(true)),
        arguments: &[],
        inputs: &[],
    };
    struct Apply;
    impl StateTransformer for Apply {
        type Appliance = Device;
        type Arguments = (bool,);
        type Error = ();
        const DEFINITION: Operation<Device> = Operation {
            precondition: Predicate {
                definition: &TRUE,
                inputs: &[],
            },
            effect: Effect {
                inputs: &[],
                outputs: &[FIRST.definition()],
                results: &[Expr::Argument(0)],
                arguments: Self::Arguments::TYPES,
            },
        };
        fn invoke(a: &mut Device, (v,): (bool,)) -> Result<(), ()> {
            if v {
                return Ok(());
            } // Accepted request without an observation.
            FIRST.update(a, |a| a[0] = Some(false));
            Err(()) // An error must not suppress a partial physical update.
        }
    }
    static APPLY: TransformerHandle<Device, ()> = TransformerHandle::new::<Apply>();
    static SURFACE: Surface<Device, ()> = Surface {
        states: &[&FIRST, &SECOND],
        transformers: &[&APPLY],
        axioms: &[],
        invariants: &[],
    };
    fn reference<T>(v: &T) -> u64 {
        ptr::from_ref(v) as usize as u64
    }
    fn initialize() -> (TaskContext, &'static Appliance<Device, ()>) {
        crate::port::test_port_reset();
        super::super::change::reset();
        FIRST.set_dependents(None);
        let context = unsafe { TaskContext::assume() };
        let owner = std::boxed::Box::leak(std::boxed::Box::new(Appliance::new(&SURFACE)));
        owner.initialize(&context, [None; 2]);
        super::super::change::bind(owner);
        (context, owner)
    }

    #[test]
    fn native_admission() {
        let (context, owner) = initialize();
        let handle = reference(&APPLY);
        for (target, args, expected) in [
            (
                0,
                &[Value::boolean(true)][..],
                InvokeStatus::UnknownTransformer,
            ),
            (handle, &[][..], InvokeStatus::Arguments),
            (
                handle,
                &[Value::integer(crate::ir::Integer::U8, 1).unwrap()][..],
                InvokeStatus::Arguments,
            ),
            (handle, &[Value::boolean(true)][..], InvokeStatus::Invoked),
        ] {
            assert_eq!(owner.invoke(&context, target, args), expected);
            assert!(!super::super::change::pending());
            assert_eq!(
                owner.read(&context, reference(FIRST.definition())),
                StateReadResult::Value {
                    sequence: 0,
                    value: None
                }
            );
        }
        owner.with(&context, |_| {
            assert_eq!(
                owner.invoke(&context, handle, &[Value::boolean(true)]),
                InvokeStatus::Busy
            );
            assert_eq!(
                owner.read(&context, reference(FIRST.definition())),
                StateReadResult::Busy
            );
        });
        for target in [0, handle, reference(&Slot::<0>)] {
            assert_eq!(
                owner.read(&context, target),
                StateReadResult::UnknownAttribute
            );
        }
        assert_eq!(
            owner.invoke(&context, handle, &[Value::boolean(false)]),
            InvokeStatus::ApplianceError
        );
        for _ in 0..2 {
            assert_eq!(
                owner.read(&context, reference(FIRST.definition())),
                StateReadResult::Value {
                    sequence: 1,
                    value: Some(Value::boolean(false))
                }
            );
        }
        let delta = super::super::change::take().unwrap();
        assert_eq!(
            (delta.sequence, delta.value),
            (1, Some(Value::boolean(false)))
        );
        assert!(!super::super::change::pending());
        super::super::change::reset();
    }

    #[test]
    fn report_order() {
        let (context, owner) = initialize();
        static NEXT: Dependent<Device> = Dependent {
            activate: |a, _, _| SECOND.update(a, |a| a[1] = Some(false)),
            next: None,
        };
        FIRST.set_dependents(Some(&NEXT));
        owner.with(&context, |a| {
            observation::update(a, &[&FIRST, &SECOND], |a| *a = [Some(true); 2])
        });
        for (sequence, attribute, value) in [
            (1, FIRST.definition(), true),
            (2, SECOND.definition(), true),
            (3, SECOND.definition(), false),
        ] {
            let delta = super::super::change::take().unwrap();
            assert_eq!(
                (delta.sequence, delta.attribute, delta.value),
                (sequence, reference(attribute), Some(Value::boolean(value)))
            );
        }
        assert!(!super::super::change::pending());
        FIRST.set_dependents(None);
        super::super::change::reset();
    }
}
