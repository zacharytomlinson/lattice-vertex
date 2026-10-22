extern crate std;
use super::*;
use crate::{
    ir::{Effect, Expr},
    surface::{
        Definition, Operation, Predicate, State, StateHandle, StateTransformer, TransformerHandle,
    },
};
use core::sync::atomic::{AtomicU8, Ordering::Relaxed};
struct Device([AtomicU8; 2]);
struct Input<const I: usize>;
impl<const I: usize> State for Input<I> {
    type Appliance = Device;
    type Value = bool;
    fn read(a: &Device) -> Option<bool> {
        match a.0[I].load(Relaxed) {
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
        a.0[0].store(1, Relaxed);
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
    crate::port::test_port_reset();
    super::super::change::reset();
    let context = unsafe { TaskContext::assume() };
    let owner = std::boxed::Box::leak(std::boxed::Box::new(Appliance::new(&SURFACE)));
    owner.initialize(&context, Device([AtomicU8::new(0), AtomicU8::new(0)]));
    super::super::change::bind(owner);
    owner.acquire(&context);
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
            for (field, value) in a.0.iter().zip(physical) {
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
        owner.initialize(&context, Device([AtomicU8::new(2), AtomicU8::new(1)]));
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
