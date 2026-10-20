//! Capacity, admission, ownership, ordering and publication boundaries.
extern crate std;
use self::std::{boxed::Box, vec::Vec};
use super::*;
use crate::{
    ir::{Expr, Type},
    kernel::{
        appliance::Native,
        change,
        peer::{exposure::Description, frame::StateReadResult},
        Appliance,
    },
    observation::{self, Dependent, Previous},
    surface::{Definition, State, Surface},
    sync::TaskContext,
};

type Device = Option<u8>;
struct Level;
impl State for Level {
    type Appliance = Device;
    type Value = u8;
    fn read(a: &Device) -> Device {
        *a
    }
}
static SOURCE: StateHandle<Device> = StateHandle::new::<Level>();
static ALIAS: StateHandle<Device> = StateHandle::new::<Level>();
static INPUTS: [&ir::Attribute<Read<Device>>; 1] = [SOURCE.definition()];
static SURFACE: Surface<Device, ()> = Surface {
    states: &[&SOURCE],
    transformers: &[],
    axioms: &[],
    invariants: &[],
};
// The predicate is opaque to maintenance: checking correspondence with native
// code belongs to the compiler. This suite tests storage and dispatch boundaries.
static RULE: Predicate<Device> = Predicate {
    definition: &Definition {
        body: Expr::Constant(Value::boolean(true)),
        arguments: &[],
        inputs: &[],
    },
    inputs: &[],
};
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}
fn difference(h: &History<u8, 2>) -> Option<i16> {
    Some(i16::from(h.value(0)?) - i16::from(h.value(1)?))
}
fn reference<T>(v: &T) -> u64 {
    ptr::from_ref(v) as usize as u64
}
fn value(v: u8) -> Option<Value> {
    Some(v.into_value())
}

struct Fixture {
    context: TaskContext,
    owner: &'static Appliance<Device, ()>,
    state: &'static RetainedState<Device, u8, i16, 2>,
}
impl Fixture {
    fn new() -> Self {
        crate::port::test_port_reset();
        change::reset();
        SOURCE.set_dependents(None);
        ALIAS.set_dependents(None);
        Self {
            context: unsafe { TaskContext::assume() },
            owner: leak(Appliance::new(&SURFACE)),
            state: leak(RetainedState::new(&SOURCE)),
        }
    }
    fn initialize(&self) {
        self.owner.initialize(&self.context, None);
    }
    fn install(&self) -> Result<(), InstallError> {
        self.owner
            .install_state(&self.context, self.state, difference, &RULE)
    }
    fn set(&self, v: Device) {
        self.owner
            .with(&self.context, |a| SOURCE.update(a, |a| *a = v));
    }
    fn read(&self, attr: &ir::Attribute<Read<Device>>) -> Option<Value> {
        let StateReadResult::Value { value, .. } = self.owner.read(&self.context, reference(attr))
        else {
            panic!("unreadable owned state")
        };
        value
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        change::reset();
        SOURCE.set_dependents(None);
        ALIAS.set_dependents(None);
    }
}

#[test]
fn history_bounds() {
    fn check<const N: usize>() {
        // All length-six words over absent/min/max. The oracle is the input
        // suffix, independent of the ring's indices, wrap logic and storage.
        for word in 0..3usize.pow(6) {
            let input: [_; 6] = core::array::from_fn(|i| {
                [None, Some(0u8), Some(u8::MAX)][word / 3usize.pow(i as u32) % 3]
            });
            let mut h = History::<u8, N>::new();
            assert!(h.is_empty());
            assert_eq!(h.value(0), None);
            for end in 1..=input.len() {
                h.append(input[end - 1]);
                assert_eq!(h.len(), end.min(N));
                assert!(!h.is_empty());
                for age in 0..=N {
                    assert_eq!(
                        h.value(age),
                        input[..end]
                            .iter()
                            .rev()
                            .take(N)
                            .nth(age)
                            .copied()
                            .flatten()
                    );
                }
                assert_eq!(h.value(usize::MAX), None);
            }
        }
    }
    check::<1>();
    check::<2>();
    check::<4>();
}

#[test]
fn installation_admission() {
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Phase {
        Uninitialized,
        Open,
        Bound,
        Locked,
        Published,
    }
    for (phase, expected, count) in [
        (Phase::Uninitialized, Err(InstallError::NotInitialized), 0),
        (Phase::Open, Ok(()), 1),
        (Phase::Bound, Err(InstallError::AlreadyInstalled), 1),
        (Phase::Locked, Err(InstallError::Busy), 0),
        (Phase::Published, Err(InstallError::Closed), 0),
    ] {
        let f = Fixture::new();
        if phase != Phase::Uninitialized {
            f.initialize();
        }
        if phase == Phase::Bound {
            f.install().unwrap();
        }
        if phase == Phase::Published {
            f.owner.seal();
        }
        let result = if phase == Phase::Locked {
            f.owner.with(&f.context, |_| f.install())
        } else {
            f.install()
        };
        assert_eq!(result, expected, "{phase:?}");
        assert_eq!(f.owner.installations().count(), count, "{phase:?}");
    }
    // Definition arity and input types are checked before any binding is made.
    for (arguments, inputs, bindings) in [
        (&[Type::Bool][..], &[][..], &[][..]),
        (&[][..], &[Type::Bool][..], &[][..]),
        (&[][..], &[Type::Bool][..], &INPUTS[..]),
    ] {
        let f = Fixture::new();
        f.initialize();
        let rule = leak(Predicate {
            definition: leak(Definition {
                body: Expr::Constant(Value::boolean(true)),
                arguments,
                inputs,
            }),
            inputs: bindings,
        });
        assert_eq!(
            f.owner.install_state(&f.context, f.state, difference, rule),
            Err(InstallError::Definition)
        );
        assert!(f.state.node().is_none());
        f.install().unwrap();
    }
    let f = Fixture::new();
    f.initialize();
    let hidden = leak(RetainedState::new(&ALIAS));
    assert_eq!(
        f.owner.install_state(&f.context, hidden, difference, &RULE),
        Err(InstallError::UnknownSource)
    );
    let wrong = leak(RetainedState::<Device, i16, i16, 2>::new(&SOURCE));
    assert_eq!(
        f.owner.install_state(&f.context, wrong, |_| None, &RULE),
        Err(InstallError::Type)
    );
    assert_eq!(f.owner.installations().count(), 0);
}

#[test]
fn maintenance_deltas() {
    let f = Fixture::new();
    f.initialize();
    f.install().unwrap();
    change::bind(f.owner);
    let mut accepted = Vec::new();
    let attrs = [f.state.sample(0), f.state.sample(1), f.state.result()];
    // Equality, extrema, loss/recovery, and explicitly qualified repeats.
    for (next, explicit) in [
        (Some(0), false),
        (Some(0), false),
        (Some(255), false),
        (Some(0), false),
        (None, false),
        (None, true),
        (Some(10), false),
        (Some(10), true),
    ] {
        let previous = f.read(SOURCE.definition());
        let before = attrs.map(|a| f.read(a));
        f.owner.with(&f.context, |a| {
            if explicit {
                *a = next;
                SOURCE.changed(a);
            } else {
                SOURCE.update(a, |a| *a = next);
            }
        });
        let changed = explicit || previous != next.map(Scalar::into_value);
        if changed {
            accepted.push(next);
        }
        let newest = |age| accepted.iter().rev().nth(age).copied().flatten();
        let expected = [
            newest(0).map(Scalar::into_value),
            newest(1).map(Scalar::into_value),
            newest(0)
                .zip(newest(1))
                .map(|(a, b)| (i16::from(a) - i16::from(b)).into_value()),
        ];
        let mut deltas = Vec::new();
        if changed {
            deltas.push((reference(SOURCE.definition()), next.map(Scalar::into_value)));
        }
        for ((attr, old), new) in attrs.into_iter().zip(before).zip(expected) {
            assert_eq!(f.read(attr), new);
            if old != new {
                deltas.push((reference(attr), new));
            }
        }
        assert_eq!(
            core::iter::from_fn(change::take)
                .map(|d| (d.attribute, d.value))
                .collect::<Vec<_>>(),
            deltas
        );
        assert_eq!(change::evicted(), 0);
    }
}

#[test]
fn instance_ownership() {
    let f = Fixture::new();
    f.initialize();
    f.install().unwrap();
    let second = leak(RetainedState::new(&SOURCE));
    f.owner
        .install_state(&f.context, second, difference, &RULE)
        .unwrap();
    let other = leak(Appliance::new(&SURFACE));
    other.initialize(&f.context, None);
    assert_eq!(
        other.install_state(&f.context, f.state, difference, &RULE),
        Err(InstallError::AlreadyInstalled)
    );
    change::bind(f.owner);
    for owned in [false, true] {
        let owner = if owned { f.owner } else { other };
        owner.with(&f.context, |a| SOURCE.update(a, |a| *a = Some(31)));
        for record in [f.state, second] {
            assert_eq!(
                f.read(record.sample(0)),
                if owned { value(31) } else { None }
            );
        }
        assert_eq!(change::pending(), owned);
    }
    SOURCE.set_dependents(None);
    f.set(Some(32));
    for record in [f.state, second] {
        assert_eq!(f.read(record.result()), Some(1i16.into_value()));
        other.with(&f.context, |a| {
            assert_eq!(record.result().location.read(a), None)
        });
    }
    f.owner.with(&f.context, |_| {
        assert_eq!(
            f.owner.read(&f.context, reference(f.state.result())),
            StateReadResult::Busy
        )
    });
}

#[test]
fn transition_order() {
    let f = Fixture::new();
    f.initialize();
    f.install().unwrap();
    static REENTER: Dependent<Device> = Dependent {
        activate: |a, _, _| SOURCE.update(a, |a| *a = Some(20)),
        next: None,
    };
    static SEEN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static CHECK: Dependent<Device> = Dependent {
        activate: |_, _, t| {
            let expected = [
                Transition {
                    before: Previous::Captured(value(10)),
                    after: value(20),
                },
                Transition {
                    before: Previous::Captured(None),
                    after: value(10),
                },
                Transition {
                    before: Previous::Unknown,
                    after: value(20),
                },
            ];
            assert_eq!(t, expected[SEEN.fetch_add(1, Ordering::Relaxed)]);
        },
        next: None,
    };
    SEEN.store(0, Ordering::Relaxed);
    ALIAS.set_dependents(Some(&REENTER));
    SOURCE.set_dependents(Some(&CHECK));
    // History order and captured callbacks must survive a reentrant report.
    f.owner.with(&f.context, |a| {
        observation::update(a, &[&ALIAS, &SOURCE], |a| *a = Some(10))
    });
    for (attr, expected) in [
        (f.state.sample(0), value(20)),
        (f.state.sample(1), value(10)),
        (f.state.result(), Some(10i16.into_value())),
    ] {
        assert_eq!(f.read(attr), expected);
    }
    f.owner.with(&f.context, |a| SOURCE.changed(a));
    assert_eq!(SEEN.load(Ordering::Relaxed), 3);
}

#[test]
fn publication_identity() {
    let f = Fixture::new();
    f.initialize();
    let base = Description::new(f.owner.surface()).unwrap();
    f.install().unwrap();
    let description = Description::with_installed(f.owner).unwrap();
    assert_eq!(crate::ir::Surface::states(f.owner).count(), 4);
    assert_eq!(crate::ir::Surface::axioms(f.owner).count(), 1);
    let mut bytes = std::vec![0; description.len() as usize];
    description.encode_part(0, &mut bytes);
    assert!(description.len() > base.len());
    for (entry, attr) in bytes[13..13 + 4 * 9]
        .chunks_exact(9)
        .zip(crate::ir::Surface::states(f.owner))
    {
        assert_eq!(
            u64::from_le_bytes(entry[..8].try_into().unwrap()),
            reference(attr)
        );
    }
    #[cfg(feature = "firmware-update")]
    assert_ne!(
        crate::kernel::update::surface_identity(f.owner.surface()),
        crate::kernel::update::appliance_surface_identity(f.owner)
    );
    for v in [None, Some(0), Some(255)] {
        f.set(v);
        let mut current = std::vec![0; bytes.len()];
        description.encode_part(0, &mut current);
        assert_eq!(
            current, bytes,
            "values cannot change the published definition"
        );
        assert_eq!(f.install(), Err(InstallError::Closed));
    }
}
