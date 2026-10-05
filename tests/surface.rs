use lattice_vertex::{
    ir::{Binary, Effect, Expr, Integer, Type, Value},
    surface::{
        ArgumentError, Arguments, Definition, InvokeError, Operation, Predicate, Scalar, State,
        StateHandle, StateTransformer, Surface, TransformerHandle,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HardwareError {
    Disconnected,
}

#[derive(Default)]
struct Appliance {
    power: Option<bool>,
    level: Option<u8>,
    calls: usize,
    disconnected: bool,
    requested: Option<(bool, u8)>,
}

struct Power;
impl State for Power {
    type Appliance = Appliance;
    type Value = bool;
    fn read(appliance: &Appliance) -> Option<bool> {
        appliance.power
    }
}
struct Level;
impl State for Level {
    type Appliance = Appliance;
    type Value = u8;
    fn read(appliance: &Appliance) -> Option<u8> {
        appliance.level
    }
}

static POWER: StateHandle<Appliance> = StateHandle::new::<Power>();
static LEVEL: StateHandle<Appliance> = StateHandle::new::<Level>();
const ZERO: Value = match Value::integer(Integer::U8, 0) {
    Ok(value) => value,
    Err(_) => panic!("invalid literal"),
};
const LIMIT: Value = match Value::integer(Integer::U8, 200) {
    Ok(value) => value,
    Err(_) => panic!("invalid literal"),
};
static COUPLING_DEFINITION: Definition<Appliance> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Input(0),
        &Expr::Binary(Binary::Greater, &Expr::Input(1), &Expr::Constant(ZERO)),
    ),
    arguments: &[],
    inputs: <(bool, u8) as Arguments>::TYPES,
};
static COUPLING: Predicate<Appliance> = Predicate {
    definition: &COUPLING_DEFINITION,
    inputs: &[POWER.definition(), LEVEL.definition()],
};
static SAFE_LEVEL: Definition<Appliance> = Definition {
    body: Expr::Binary(Binary::LessEqual, &Expr::Input(0), &Expr::Constant(LIMIT)),
    arguments: &[],
    inputs: <(u8,) as Arguments>::TYPES,
};
static INVARIANT: Predicate<Appliance> = Predicate {
    definition: &SAFE_LEVEL,
    inputs: &[LEVEL.definition()],
};
static PERMITTED: Definition<Appliance> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: &[],
    inputs: &[],
};

struct Set;
impl StateTransformer for Set {
    type Appliance = Appliance;
    type Arguments = (bool, u8);
    type Error = HardwareError;
    const DEFINITION: Operation<Appliance> = Operation {
        precondition: Predicate {
            definition: &PERMITTED,
            inputs: &[],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[POWER.definition(), LEVEL.definition()],
            results: &[Expr::Argument(0), Expr::Argument(1)],
            arguments: Self::Arguments::TYPES,
        },
    };

    fn invoke(appliance: &mut Appliance, arguments: (bool, u8)) -> Result<(), HardwareError> {
        appliance.calls += 1;
        if appliance.disconnected {
            return Err(HardwareError::Disconnected);
        }
        appliance.requested = Some(arguments);
        Ok(())
    }
}

static SET: TransformerHandle<Appliance, HardwareError> = TransformerHandle::new::<Set>();

// A custom decoder cannot bypass the kernel's scalar signature check.
struct CustomArguments;
impl Arguments for CustomArguments {
    const TYPES: &'static [Type] = <(bool, u8) as Arguments>::TYPES;
    fn decode(_: &[Value]) -> Result<Self, ArgumentError> {
        Ok(Self)
    }
}
struct CustomSet;
impl StateTransformer for CustomSet {
    type Appliance = Appliance;
    type Arguments = CustomArguments;
    type Error = HardwareError;
    const DEFINITION: Operation<Appliance> = Set::DEFINITION;
    fn invoke(appliance: &mut Appliance, _: CustomArguments) -> Result<(), HardwareError> {
        appliance.calls += 1;
        Ok(())
    }
}
static CUSTOM_SET: TransformerHandle<Appliance, HardwareError> =
    TransformerHandle::new::<CustomSet>();

static SURFACE: Surface<'static, Appliance, HardwareError> = Surface {
    states: &[&POWER, &LEVEL],
    transformers: &[&SET],
    axioms: &[&COUPLING],
    invariants: &[&INVARIANT],
};

fn scalar(integer: Integer, value: i128) -> Value {
    Value::integer(integer, value).unwrap()
}

#[test]
fn enumeration_preserves_device_constraints_and_their_original_state_references() {
    let definition = SURFACE.transformers[0].definition();
    for (state, affected) in SURFACE.states.iter().zip(definition.effect.outputs) {
        assert!(core::ptr::eq(state.definition(), *affected));
    }
    assert_eq!(SURFACE.axioms.len(), 1);
    assert_eq!(SURFACE.invariants.len(), 1);
    assert!(core::ptr::eq(
        SURFACE.axioms[0].definition,
        &COUPLING_DEFINITION
    ));
    assert!(core::ptr::eq(SURFACE.invariants[0].definition, &SAFE_LEVEL));
    assert!(core::ptr::eq(
        definition.precondition.definition,
        &PERMITTED
    ));
    for predicate in SURFACE.axioms.iter().chain(SURFACE.invariants) {
        assert!(predicate.definition.arguments.is_empty());
        for (state, ty) in predicate.inputs.iter().zip(predicate.definition.inputs) {
            assert_eq!(&state.ty, ty);
            assert!(SURFACE
                .states
                .iter()
                .any(|s| core::ptr::eq(s.definition(), *state)));
        }
    }

    let mut appliance = Appliance::default();
    for (power, level) in [
        (None, None),
        (Some(false), Some(0)),
        (Some(true), Some(173)),
    ] {
        appliance.power = power;
        appliance.level = level;
        assert_eq!(POWER.read(&appliance), power.map(Value::boolean));
        assert_eq!(
            LEVEL.read(&appliance),
            level.map(|n| scalar(Integer::U8, i128::from(n)))
        );
    }
}

#[test]
fn malformed_arguments_never_reach_appliance() {
    let valid = [Value::boolean(true), scalar(Integer::U8, 128)];
    let wrong_kind = [scalar(Integer::U8, 1), valid[1]];
    let wrong_width = [valid[0], scalar(Integer::U16, 128)];
    let wrong_sign = [valid[0], scalar(Integer::I8, 127)];
    let extra = [valid[0], valid[1], Value::boolean(false)];
    let cases: &[(&[Value], ArgumentError)] = &[
        (
            &[],
            ArgumentError::Count {
                expected: 2,
                actual: 0,
            },
        ),
        (
            &valid[..1],
            ArgumentError::Count {
                expected: 2,
                actual: 1,
            },
        ),
        (
            &extra,
            ArgumentError::Count {
                expected: 2,
                actual: 3,
            },
        ),
        (
            &wrong_kind,
            ArgumentError::Type {
                index: 0,
                expected: Type::Bool,
                actual: Type::Integer(Integer::U8),
            },
        ),
        (
            &wrong_width,
            ArgumentError::Type {
                index: 1,
                expected: Type::Integer(Integer::U8),
                actual: Type::Integer(Integer::U16),
            },
        ),
        (
            &wrong_sign,
            ArgumentError::Type {
                index: 1,
                expected: Type::Integer(Integer::U8),
                actual: Type::Integer(Integer::I8),
            },
        ),
    ];
    for &(arguments, error) in cases {
        for transformer in [&SET, &CUSTOM_SET] {
            let mut appliance = Appliance::default();
            assert_eq!(
                transformer.invoke(&mut appliance, arguments),
                Err(InvokeError::Arguments(error))
            );
            assert_eq!(appliance.calls, 0);
            assert_eq!(appliance.requested, None);
        }
    }
}

#[test]
fn invocation_is_once_and_does_not_turn_modeled_effects_into_observations() {
    for disconnected in [false, true] {
        let mut appliance = Appliance {
            power: Some(false),
            level: Some(0),
            disconnected,
            ..Appliance::default()
        };
        let arguments = [Value::boolean(true), scalar(Integer::U8, 128)];
        let expected = if disconnected {
            Err(InvokeError::Appliance(HardwareError::Disconnected))
        } else {
            Ok(())
        };
        assert_eq!(SET.invoke(&mut appliance, &arguments), expected);
        assert_eq!(appliance.calls, 1);
        assert_eq!(appliance.requested, (!disconnected).then_some((true, 128)));
        assert_eq!(POWER.read(&appliance), Some(Value::boolean(false)));
        assert_eq!(LEVEL.read(&appliance), Some(ZERO));
    }
}

#[test]
fn native_scalar_and_argument_products_preserve_width_signedness_and_extremes() {
    fn roundtrip<T: Scalar + core::fmt::Debug + PartialEq>(values: &[T]) {
        for &value in values {
            let encoded = value.into_value();
            assert_eq!(encoded.ty(), T::TYPE);
            assert_eq!(T::from_value(encoded), Some(value));
        }
    }
    roundtrip(&[false, true]);
    roundtrip(&[u8::MIN, u8::MAX]);
    roundtrip(&[u16::MIN, u16::MAX]);
    roundtrip(&[u32::MIN, u32::MAX]);
    roundtrip(&[u64::MIN, u64::MAX]);
    roundtrip(&[i8::MIN, i8::MAX]);
    roundtrip(&[i16::MIN, i16::MAX]);
    roundtrip(&[i32::MIN, i32::MAX]);
    roundtrip(&[i64::MIN, i64::MAX]);
    assert_eq!(bool::from_value(1_u8.into_value()), None);
    assert_eq!(u8::from_value(1_u16.into_value()), None);
    assert_eq!(u8::from_value(1_i8.into_value()), None);
    assert_eq!(<()>::decode(&[]), Ok(()));
    assert_eq!(<(u8,)>::decode(&[255_u8.into_value()]), Ok((255,)));
    assert_eq!(
        <(bool, u64, i64)>::decode(&[
            true.into_value(),
            u64::MAX.into_value(),
            i64::MIN.into_value()
        ]),
        Ok((true, u64::MAX, i64::MIN))
    );
    assert_eq!(
        <(bool, u64, i64, u8)>::decode(&[
            true.into_value(),
            u64::MAX.into_value(),
            i64::MIN.into_value(),
            255_u8.into_value()
        ]),
        Ok((true, u64::MAX, i64::MIN, 255))
    );
    assert_eq!(
        <[u16; 5]>::decode(&[u16::MAX.into_value(); 5]),
        Ok([u16::MAX; 5])
    );
    assert_eq!(<[u16; 0]>::decode(&[]), Ok([]));
}

struct BadEffect;
impl StateTransformer for BadEffect {
    type Appliance = Appliance;
    type Arguments = (bool, i8);
    type Error = HardwareError;
    const DEFINITION: Operation<Appliance> = Set::DEFINITION;
    fn invoke(_: &mut Appliance, _: Self::Arguments) -> Result<(), HardwareError> {
        unreachable!()
    }
}
static BAD_PRECONDITION: Definition<Appliance> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: <(bool,) as Arguments>::TYPES,
    inputs: &[],
};
struct BadPrecondition;
impl StateTransformer for BadPrecondition {
    type Appliance = Appliance;
    type Arguments = (bool, u8);
    type Error = HardwareError;
    const DEFINITION: Operation<Appliance> = Operation {
        precondition: Predicate {
            definition: &BAD_PRECONDITION,
            inputs: &[],
        },
        effect: Set::DEFINITION.effect,
    };
    fn invoke(_: &mut Appliance, _: Self::Arguments) -> Result<(), HardwareError> {
        unreachable!()
    }
}

#[test]
fn inconsistent_native_and_mathematical_signatures_cannot_construct_a_handle() {
    let invalid = [
        TransformerHandle::new::<BadEffect>,
        TransformerHandle::new::<BadPrecondition>,
    ];
    for construct in invalid {
        assert!(std::panic::catch_unwind(construct).is_err());
    }
}

#[test]
fn shared_ir_view_keeps_distinct_callables_even_when_their_definitions_match() {
    let transformers = [&SET, &CUSTOM_SET];
    let surface = Surface {
        states: SURFACE.states,
        transformers: &transformers,
        axioms: SURFACE.axioms,
        invariants: SURFACE.invariants,
    };
    let mut definitions = lattice_vertex::ir::Surface::transformers(&surface);
    let first = definitions.next().unwrap();
    let second = definitions.next().unwrap();
    assert!(!core::ptr::eq(first, second));
    assert!(core::ptr::eq(first, SET.definition()));
    assert!(core::ptr::eq(second, CUSTOM_SET.definition()));
    assert!(core::ptr::eq(
        first.effect.outputs[0],
        second.effect.outputs[0]
    ));
    // The original native handles still select their own implementations.
    let mut appliance = Appliance::default();
    SET.invoke(
        &mut appliance,
        &[Value::boolean(true), scalar(Integer::U8, 100)],
    )
    .unwrap();
    assert_eq!(appliance.requested, Some((true, 100)));
    appliance.requested = None;
    CUSTOM_SET
        .invoke(
            &mut appliance,
            &[Value::boolean(true), scalar(Integer::U8, 100)],
        )
        .unwrap();
    assert_eq!(appliance.calls, 2);
    assert_eq!(appliance.requested, None);
}
