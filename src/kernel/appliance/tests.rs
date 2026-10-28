extern crate std;
use super::acquisition::{Acquisition, AcquisitionEvent, Schedule};
use super::*;
use crate::{
    ir::{Effect, Expr},
    surface::{
        Definition, Operation, Predicate, State, StateHandle, StateTransformer, TransformerHandle,
    },
};
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering::Relaxed};
struct Device {
    values: [AtomicU8; 2],
    reads: [AtomicU32; 2],
    sources: [Acquisition; 2],
}
impl Device {
    fn new(values: [u8; 2], sources: [Acquisition; 2]) -> Self {
        Self {
            values: values.map(AtomicU8::new),
            reads: [AtomicU32::new(0), AtomicU32::new(0)],
            sources,
        }
    }
    fn reads(&self) -> [u32; 2] {
        self.reads.each_ref().map(|n| n.load(Relaxed))
    }
}
struct Input<const I: usize>;
impl<const I: usize> State for Input<I> {
    type Appliance = Device;
    type Value = bool;
    fn acquisition(a: &'static Device) -> Acquisition {
        a.sources[I]
    }
    fn read(a: &Device) -> Option<bool> {
        a.reads[I].fetch_add(1, Relaxed);
        match a.values[I].load(Relaxed) {
            0 => None,
            n => Some(n == 2),
        }
    }
}
static A: StateHandle<Device> = StateHandle::new::<Input<0>>("a");
static B: StateHandle<Device> = StateHandle::new::<Input<1>>("b");
static TRUE: Definition<Device> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: &[],
    inputs: &[],
};
struct Set;
impl StateTransformer for Set {
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
            outputs: &[A.definition()],
            results: &[Expr::Argument(0)],
            arguments: &[crate::ir::Type::Bool],
        },
    };
    fn invoke(a: &Device, (v,): (bool,)) -> Result<(), ()> {
        if v {
            return Ok(());
        }
        a.values[0].store(1, Relaxed);
        Err(()) // An actual change survives a hardware error.
    }
}
static SET: TransformerHandle<Device, ()> = TransformerHandle::new::<Set>("set");
static SURFACE: Surface<Device, ()> = Surface {
    states: &[&A, &B],
    transformers: &[&SET],
    axioms: &[],
    invariants: &[],
};
fn owner() -> (TaskContext, &'static Appliance<Device, ()>) {
    let (context, owner) = initialize([Acquisition::periodic(10); 2]);
    owner.acquire(&context);
    (context, owner)
}
fn initialize(sources: [Acquisition; 2]) -> (TaskContext, &'static Appliance<Device, ()>) {
    crate::port::test_port_reset();
    super::super::change::reset();
    let context = unsafe { TaskContext::assume() };
    let owner = std::boxed::Box::leak(std::boxed::Box::new(Appliance::new(&SURFACE)));
    owner.initialize(&context, Device::new([0, 0], sources));
    super::super::change::bind(owner);
    (context, owner)
}
#[test]
fn acquisition_transitions() {
    let (context, owner) = owner();
    for (physical, changes) in [
        ([0, 0], 0),
        ([2, 1], 2),
        ([2, 1], 0),
        ([1, 0], 2),
        ([0, 0], 1),
    ] {
        owner.with(&context, |a| {
            for (field, value) in a.values.iter().zip(physical) {
                field.store(value, Relaxed);
            }
        });
        let before = super::super::change::sequence();
        owner.acquire(&context);
        assert_eq!(
            super::super::change::sequence().wrapping_sub(before),
            changes
        );
        let emitted: std::vec::Vec<_> = core::iter::from_fn(super::super::change::take).collect();
        assert_eq!(emitted.len(), changes as usize);
        for (state, raw) in [&A, &B].into_iter().zip(physical) {
            let expected = (raw != 0).then_some(Value::boolean(raw == 2));
            assert_eq!(
                owner.read(&context, state.reference()),
                StateReadResult::Value {
                    sequence: before + changes,
                    value: expected
                }
            );
        }
        assert!(!super::super::change::pending());
    }
    super::super::change::reset();
}
#[test]
fn native_admission() {
    let (context, owner) = owner();
    for (handle, args, status) in [
        (
            0,
            &[Value::boolean(true)][..],
            InvokeStatus::UnknownTransformer,
        ),
        (
            ptr::from_ref(&SET) as usize as u64,
            &[Value::boolean(true)][..],
            InvokeStatus::UnknownTransformer,
        ),
        (SET.reference(), &[][..], InvokeStatus::Arguments),
        (
            SET.reference(),
            &[Value::integer(crate::ir::Integer::U8, 1).unwrap()][..],
            InvokeStatus::Arguments,
        ),
        (
            SET.reference(),
            &[Value::boolean(true)][..],
            InvokeStatus::Invoked,
        ),
        (
            SET.reference(),
            &[Value::boolean(false)][..],
            InvokeStatus::ApplianceError,
        ),
    ] {
        assert_eq!(owner.invoke(&context, handle, args), status);
    }
    assert_eq!(
        super::super::change::take().unwrap().value,
        Some(Value::boolean(false))
    );
    assert!(!super::super::change::pending());
    assert_eq!(
        owner.read(&context, SET.reference()),
        StateReadResult::UnknownAttribute
    );
    // Only the short acquisition lock excludes another acquisition/read, not
    // the shared native adapter access used by a hardware function.
    owner.with(&context, |_| {
        owner.acquire(&context);
        assert!(matches!(
            owner.read(&context, A.reference()),
            StateReadResult::Value { .. }
        ));
    });
    super::super::change::reset();
}

#[test]
fn binding_resolution() {
    let mut reference = None;
    for index in [0, 1] {
        let handle = std::boxed::Box::leak(std::boxed::Box::new(if index == 0 {
            StateHandle::new::<Input<0>>("input")
        } else {
            StateHandle::new::<Input<1>>("input")
        }));
        let surface = std::boxed::Box::leak(std::boxed::Box::new(Surface::<Device, ()> {
            states: std::boxed::Box::leak(std::boxed::Box::new([&*handle])),
            transformers: &[],
            axioms: &[],
            invariants: &[],
        }));
        crate::port::test_port_reset();
        let context = unsafe { TaskContext::assume() };
        let owner = std::boxed::Box::leak(std::boxed::Box::new(Appliance::new(surface)));
        owner.initialize(
            &context,
            Device::new([2, 1], [Acquisition::periodic(10); 2]),
        );
        owner.acquire(&context);
        let saved = *reference.get_or_insert(handle.reference());
        assert_eq!(saved, handle.reference());
        assert!(
            matches!(owner.read(&context, saved), StateReadResult::Value { value: Some(v), .. } if v == Value::boolean(index == 0))
        );
        assert_eq!(
            owner.read(&context, ptr::from_ref(handle.definition()) as usize as u64),
            StateReadResult::UnknownAttribute
        );
    }
}

#[test]
fn acquisition_selection_and_deadlines() {
    for (shared, periodic) in [(false, false), (true, false), (false, true)] {
        let event = std::boxed::Box::leak(std::boxed::Box::new(AcquisitionEvent::new()));
        let second = if shared {
            Acquisition::Event(event)
        } else if periodic {
            Acquisition::periodic(50)
        } else {
            Acquisition::OnInvocation
        };
        let (context, owner) = initialize([Acquisition::Event(event), second]);
        let signals = owner.acquisition_signals().unwrap();
        let mut schedule = Schedule::new(owner, signals);
        let start = u32::MAX - 20;
        let mask = schedule.due(start, signals);
        assert_eq!(mask, 3);
        assert!(owner.acquire_selected(&context, mask));
        schedule.completed(mask, start);
        assert_eq!(owner.native().reads(), [1, 1]);
        assert_eq!(super::super::change::sequence(), 0); // baseline, not a change
        assert_eq!(schedule.wait_ms(start, signals), periodic.then_some(50));
        for _ in 0..3 {
            event.notify();
        } // readiness coalesces, no getter in ISR
        owner.native().values[0].store(2, Relaxed);
        owner.native().values[1].store(1, Relaxed);
        assert_eq!(owner.native().reads(), [1, 1]);
        let mask = schedule.due(start.wrapping_add(1), signals);
        assert_eq!(mask, if shared { 3 } else { 1 });
        // Another IRQ while this acquisition is in progress must survive completion.
        event.waker().wake_by_ref();
        assert!(owner.acquire_selected(&context, mask));
        schedule.completed(mask, start.wrapping_add(1));
        assert_eq!(owner.native().reads(), [2, if shared { 2 } else { 1 }]);
        let changes = if shared { 2 } else { 1 };
        assert_eq!(super::super::change::sequence(), changes);
        assert_eq!(schedule.wait_ms(start.wrapping_add(1), signals), Some(0));
        let mask = schedule.due(start.wrapping_add(2), signals);
        assert!(owner.acquire_selected(&context, mask));
        schedule.completed(mask, start.wrapping_add(2));
        assert_eq!(super::super::change::sequence(), changes); // unchanged read emits nothing
        assert_eq!(schedule.due(start.wrapping_add(49), signals), 0);
        assert_eq!(
            schedule.due(start.wrapping_add(50), signals),
            if periodic { 2 } else { 0 }
        );
        if periodic {
            assert!(owner.acquire_selected(&context, 2));
            schedule.completed(2, start.wrapping_add(50));
            assert_eq!(super::super::change::sequence(), 2);
            assert_eq!(schedule.wait_ms(start.wrapping_add(50), signals), Some(50));
        } else {
            assert_eq!(schedule.wait_ms(start.wrapping_add(50), signals), None);
        }
        // Cache contention retains readiness for retry; no committed delta is consumed.
        event.notify();
        let mask = schedule.due(100, signals);
        let guard = owner.observations().try_lock(&context).unwrap();
        assert!(!owner.acquire_selected(&context, mask));
        assert!(matches!(
            owner.read(&context, A.reference()),
            StateReadResult::Busy
        ));
        assert!(owner.cache_waiting.load(Relaxed));
        signals.retain(mask);
        drop(guard);
        assert_eq!(schedule.due(100, signals) & mask, mask);
        assert!(owner.acquire_selected(&context, mask));
        assert!(!owner.cache_waiting.load(Relaxed));
        super::super::change::reset();
    }
}
