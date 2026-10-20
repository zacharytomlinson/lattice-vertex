//! Admission, one activation per delta, operand capture and owner isolation.
extern crate std;
use self::std::boxed::Box;
use super::*;
use crate::{
    ir::{Effect, Expr},
    kernel::{
        state::{History, RetainedState},
        Appliance,
    },
    observation::{self, Dependent, Previous},
    surface::{
        Arguments, Definition, Operation, Predicate, Scalar, State, StateTransformer, Surface,
    },
    sync::TaskContext,
};
#[derive(Default)]
struct Device {
    values: [Option<i16>; 2],
    calls: usize,
    last: i32,
    reject: bool,
}
struct Input<const I: usize>;
impl<const I: usize> State for Input<I> {
    type Appliance = Device;
    type Value = i16;
    fn read(a: &Device) -> Option<i16> {
        a.values[I]
    }
}
static FIRST: StateHandle<Device> = StateHandle::new::<Input<0>>();
static SECOND: StateHandle<Device> = StateHandle::new::<Input<1>>();
static HIDDEN: StateHandle<Device> = StateHandle::new::<Input<0>>();
static TRUE: Definition<Device> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: &[],
    inputs: &[],
};
static RULE: Predicate<Device> = Predicate {
    definition: &TRUE,
    inputs: &[],
};
struct Apply;
impl StateTransformer for Apply {
    type Appliance = Device;
    type Arguments = (i32,);
    type Error = ();
    const DEFINITION: Operation<Device> = Operation {
        precondition: Predicate {
            definition: &TRUE,
            inputs: &[],
        },
        effect: Effect {
            arguments: Self::Arguments::TYPES,
            inputs: &[],
            outputs: &[],
            results: &[],
        },
    };
    fn invoke(a: &mut Device, (value,): (i32,)) -> Result<(), ()> {
        a.calls += 1;
        a.last = value;
        if a.reject {
            Err(())
        } else {
            Ok(())
        }
    }
}
static APPLY: TransformerHandle<Device, ()> = TransformerHandle::new::<Apply>();
static HIDDEN_TARGET: TransformerHandle<Device, ()> = TransformerHandle::new::<Apply>();
static SURFACE: Surface<Device, ()> = Surface {
    states: &[&FIRST, &SECOND],
    transformers: &[&APPLY],
    axioms: &[],
    invariants: &[],
};
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}
fn difference(h: &History<i16, 2>) -> Option<i32> {
    Some(i32::from(h.value(0)?) - i32::from(h.value(1)?))
}
fn positive(_: Transition, values: &[Value; 1]) -> Option<[Value; 1]> {
    (values[0].as_integer()? > 0).then_some(*values)
}
struct Fixture {
    context: TaskContext,
    owner: &'static Appliance<Device, ()>,
    history: &'static RetainedState<Device, i16, i32, 2>,
}
impl Fixture {
    fn new() -> Self {
        crate::port::test_port_reset();
        crate::kernel::change::reset();
        FIRST.set_dependents(None);
        SECOND.set_dependents(None);
        let context = unsafe { TaskContext::assume() };
        let owner = leak(Appliance::new(&SURFACE));
        owner.initialize(&context, Device::default());
        let history = leak(RetainedState::new(&SECOND));
        owner
            .install_state(&context, history, difference, &RULE)
            .unwrap();
        Self {
            context,
            owner,
            history,
        }
    }
    fn set(&self, value: Option<i16>) {
        self.owner
            .with(&self.context, |a| SECOND.update(a, |a| a.values[1] = value));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        FIRST.set_dependents(None);
        SECOND.set_dependents(None);
        crate::kernel::change::reset();
    }
}

#[test]
fn installation_bounds() {
    let f = Fixture::new();
    let uninstalled = leak(RetainedState::<Device, i16, i32, 2>::new(&SECOND));
    for (source, input, target, expected) in [
        (
            &HIDDEN,
            f.history.result(),
            &APPLY,
            Err(InstallError::UnknownSource),
        ),
        (
            &SECOND,
            HIDDEN.definition(),
            &APPLY,
            Err(InstallError::UnknownInput),
        ),
        (
            &SECOND,
            uninstalled.result(),
            &APPLY,
            Err(InstallError::UnknownInput),
        ),
        (
            &SECOND,
            f.history.result(),
            &HIDDEN_TARGET,
            Err(InstallError::UnknownTransformer),
        ),
        (&SECOND, f.history.result(), &APPLY, Ok(())),
    ] {
        let fragment = leak(Fragment::new(source, [input], positive, target));
        assert_eq!(f.owner.install_fragment(&f.context, fragment), expected);
        if expected.is_ok() {
            assert_eq!(
                f.owner.install_fragment(&f.context, fragment),
                Err(InstallError::AlreadyInstalled)
            );
        } else {
            assert!(fragment.owner.load(Ordering::Relaxed).is_null());
        }
    }
    let wrong_count = leak(Fragment::new(&SECOND, [], |_, _| Some([]), &APPLY));
    assert_eq!(
        f.owner.install_fragment(&f.context, wrong_count),
        Err(InstallError::Arguments)
    );
    let valid = leak(Fragment::new(
        &SECOND,
        [f.history.result()],
        positive,
        &APPLY,
    ));
    f.owner.seal();
    assert_eq!(
        f.owner.install_fragment(&f.context, valid),
        Err(InstallError::Closed)
    );
}

#[test]
fn activation_bounds() {
    for (before, after, explicit, reject, expected, calls) in [
        (None, Some(2), false, false, Outcome::Unavailable, 0),
        (Some(1), None, false, false, Outcome::Unavailable, 0),
        (Some(2), Some(2), false, false, Outcome::NotActivated, 0),
        (Some(2), Some(2), true, false, Outcome::Inactive, 0),
        (Some(2), Some(1), false, false, Outcome::Inactive, 0),
        (Some(1), Some(2), false, false, Outcome::Invoked, 1),
        (
            Some(i16::MIN),
            Some(i16::MAX),
            false,
            false,
            Outcome::Invoked,
            1,
        ),
        (Some(1), Some(2), false, true, Outcome::ApplianceError, 1),
    ] {
        let f = Fixture::new();
        f.set(before);
        let fragment = leak(Fragment::new(
            &SECOND,
            [f.history.result()],
            positive,
            &APPLY,
        ));
        f.owner.install_fragment(&f.context, fragment).unwrap();
        f.owner.with(&f.context, |a| {
            a.reject = reject;
            if explicit {
                a.values[1] = after;
                SECOND.changed(a);
            } else {
                SECOND.update(a, |a| a.values[1] = after);
            }
        });
        assert_eq!(fragment.outcome(), expected);
        f.owner.with(&f.context, |a| {
            assert_eq!(a.calls, calls);
            if calls != 0 {
                assert_eq!(
                    a.last,
                    i32::from(after.unwrap()) - i32::from(before.unwrap())
                );
            }
        });
        f.set(after); // No new delta: neither successful nor failed work repeats.
        f.owner.with(&f.context, |a| assert_eq!(a.calls, calls));
    }
    let f = Fixture::new();
    let wrong_type = leak(Fragment::new(
        &SECOND,
        [],
        |_, _| Some([Value::boolean(true)]),
        &APPLY,
    ));
    f.owner.install_fragment(&f.context, wrong_type).unwrap();
    f.set(Some(1));
    assert_eq!(wrong_type.outcome(), Outcome::Arguments);
    f.owner.with(&f.context, |a| assert_eq!(a.calls, 0));
}

#[test]
fn operand_order() {
    let f = Fixture::new();
    f.set(Some(10));
    let fragment = leak(Fragment::new(
        &FIRST,
        [f.history.result()],
        positive,
        &APPLY,
    ));
    f.owner.install_fragment(&f.context, fragment).unwrap();
    f.owner.with(&f.context, |a| {
        observation::update(a, &[&FIRST, &SECOND], |a| a.values = [Some(1), Some(20)])
    });
    f.owner
        .with(&f.context, |a| assert_eq!((a.calls, a.last), (1, 10)));
    // The first callback can cause a newer report before the original second
    // trigger dispatches. Its declared source operand must remain captured.
    let f = Fixture::new();
    static REENTER: Dependent<Device> = Dependent {
        activate: |a, _, _| SECOND.update(a, |a| a.values[1] = Some(20)),
        next: None,
    };
    FIRST.set_dependents(Some(&REENTER));
    let fragment = leak(Fragment::new(
        &SECOND,
        [SECOND.definition()],
        |t, v| {
            assert_eq!(t.after, Some(v[0]));
            Some([(v[0].as_integer().unwrap() as i32).into_value()])
        },
        &APPLY,
    ));
    f.owner.install_fragment(&f.context, fragment).unwrap();
    f.owner.with(&f.context, |a| {
        observation::update(a, &[&FIRST, &SECOND], |a| a.values = [Some(1), Some(10)])
    });
    f.owner.with(&f.context, |a| {
        assert_eq!((a.calls, a.last, a.values[1]), (2, 10, Some(20)))
    });
}

#[test]
fn instance_and_list_ownership() {
    let f = Fixture::new();
    let first = leak(Fragment::new(
        &FIRST,
        [],
        |t, _| {
            assert_eq!(t.before, Previous::Captured(None));
            Some([1i32.into_value()])
        },
        &APPLY,
    ));
    let second = leak(Fragment::new(
        &FIRST,
        [],
        |_, _| Some([2i32.into_value()]),
        &APPLY,
    ));
    for fragment in [first, second] {
        f.owner.install_fragment(&f.context, fragment).unwrap();
    }
    FIRST.update(&mut Device::default(), |a| a.values[0] = Some(1));
    assert_eq!(first.outcome(), Outcome::NotActivated);
    assert_eq!(second.outcome(), Outcome::NotActivated);
    FIRST.set_dependents(None);
    f.owner
        .with(&f.context, |a| FIRST.update(a, |a| a.values[0] = Some(1)));
    f.owner
        .with(&f.context, |a| assert_eq!((a.calls, a.last), (2, 2)));
}

#[test]
fn peer_admission() {
    fn signature<const N: usize>(arguments: [Type; N], expected: Result<(), InstallError>) {
        let f = Fixture::new();
        let fragment = leak(Fragment::to_peer(
            &SECOND,
            [],
            |_, _| None,
            RemoteTransformer {
                owner: 2,
                transformer: 7,
                arguments,
            },
        ));
        assert_eq!(f.owner.install_fragment(&f.context, fragment), expected);
    }
    signature([Type::Bool; 30], Ok(()));
    signature([Type::Bool; 31], Err(InstallError::Arguments));
    signature([Type::Integer(crate::ir::Integer::I64); 6], Ok(()));
    signature(
        [Type::Integer(crate::ir::Integer::I64); 7],
        Err(InstallError::Arguments),
    );
    let f = Fixture::new();
    for (owner, transformer) in [(0, 7), (u64::MAX, 7), (2, 0), (2, u64::MAX)] {
        let fragment = leak(Fragment::to_peer(
            &SECOND,
            [],
            |_, _| Some([]),
            RemoteTransformer {
                owner,
                transformer,
                arguments: [],
            },
        ));
        assert_eq!(
            f.owner.install_fragment(&f.context, fragment),
            Err(InstallError::InvalidTarget)
        );
    }
}

#[test]
fn peer_handoff() {
    use crate::kernel::peer::continuation;
    let i32_type = Type::Integer(crate::ir::Integer::I32);
    for (ready, owner, ty, expected) in [
        (false, 2, i32_type, Outcome::NotReady),
        (true, 1, i32_type, Outcome::InvalidTarget),
        (true, 2, Type::Bool, Outcome::Arguments),
        (true, 2, i32_type, Outcome::Submitted),
    ] {
        let f = Fixture::new();
        continuation::reset();
        if ready {
            continuation::start(1, 100);
        }
        f.set(Some(10));
        let fragment = leak(Fragment::to_peer(
            &SECOND,
            [f.history.result()],
            positive,
            RemoteTransformer {
                owner,
                transformer: 7,
                arguments: [ty],
            },
        ));
        f.owner.install_fragment(&f.context, fragment).unwrap();
        f.set(Some(20));
        assert_eq!(fragment.outcome(), expected);
        assert_eq!(continuation::pending(), expected == Outcome::Submitted);
        f.owner.with(&f.context, |a| {
            assert_eq!(
                a.calls, 0,
                "peer target cannot execute against the local appliance"
            )
        });
    }
    continuation::reset();
}
