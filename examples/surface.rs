//! A lamp exposes typed states, one transformer, and its operating mathematics.
//! Run on the host with `cargo run --locked --example surface`.

use lattice_vertex::{
    ir::{Binary, Effect, Expr, Integer, Value},
    surface::{
        Arguments, Definition, Operation, Predicate, State, StateHandle, StateTransformer, Surface,
        TransformerHandle,
    },
};

// This example lamp derives power from its PWM level. Its safe limit is 200.
struct Lamp {
    level: u8,
}

struct Power;
impl State for Power {
    type Appliance = Lamp;
    type Value = bool;
    fn read(lamp: &Lamp) -> Option<bool> {
        Some(lamp.level != 0)
    }
}

struct Level;
impl State for Level {
    type Appliance = Lamp;
    type Value = u8;
    fn read(lamp: &Lamp) -> Option<u8> {
        Some(lamp.level)
    }
}

static POWER: StateHandle<Lamp> = StateHandle::new::<Power>();
static LEVEL: StateHandle<Lamp> = StateHandle::new::<Level>();
const ZERO: Value = match Value::integer(Integer::U8, 0) {
    Ok(value) => value,
    Err(_) => panic!("invalid literal"),
};
const LIMIT: Value = match Value::integer(Integer::U8, 200) {
    Ok(value) => value,
    Err(_) => panic!("invalid literal"),
};

// Device fact: power is on exactly when PWM level is nonzero.
static POWER_LEVEL: Definition<Lamp> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Input(0),
        &Expr::Binary(Binary::Greater, &Expr::Input(1), &Expr::Constant(ZERO)),
    ),
    arguments: &[],
    inputs: <(bool, u8) as Arguments>::TYPES,
};
static COUPLING: Predicate<Lamp> = Predicate {
    definition: &POWER_LEVEL,
    inputs: &[POWER.definition(), LEVEL.definition()],
};

// Device invariant: the safe PWM level may not be exceeded.
static SAFE_LEVEL: Definition<Lamp> = Definition {
    body: Expr::Binary(Binary::LessEqual, &Expr::Input(0), &Expr::Constant(LIMIT)),
    arguments: &[],
    inputs: <(u8,) as Arguments>::TYPES,
};
static LEVEL_LIMIT: Predicate<Lamp> = Predicate {
    definition: &SAFE_LEVEL,
    inputs: &[LEVEL.definition()],
};

// Per-call condition: requested power/level agree and the requested level is safe.
static PERMITTED: Definition<Lamp> = Definition {
    body: Expr::And(
        &Expr::Binary(
            Binary::Equal,
            &Expr::Argument(0),
            &Expr::Binary(Binary::Greater, &Expr::Argument(1), &Expr::Constant(ZERO)),
        ),
        &Expr::Binary(
            Binary::LessEqual,
            &Expr::Argument(1),
            &Expr::Constant(LIMIT),
        ),
    ),
    arguments: <(bool, u8) as Arguments>::TYPES,
    inputs: &[],
};

#[derive(Debug)]
struct InvalidSetting;
struct Set;
impl StateTransformer for Set {
    type Appliance = Lamp;
    type Arguments = (bool, u8);
    type Error = InvalidSetting;

    const DEFINITION: Operation<Lamp> = Operation {
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

    fn invoke(lamp: &mut Lamp, (power, level): (bool, u8)) -> Result<(), InvalidSetting> {
        if power != (level != 0) || level > 200 {
            return Err(InvalidSetting);
        }
        // Real firmware writes its PWM hardware and establishes its observation.
        lattice_vertex::observation::update(lamp, &[&POWER, &LEVEL], |lamp| lamp.level = level);
        Ok(())
    }
}

static SET: TransformerHandle<Lamp, InvalidSetting> = TransformerHandle::new::<Set>();
static SURFACE: Surface<'static, Lamp, InvalidSetting> = Surface {
    states: &[&POWER, &LEVEL],
    transformers: &[&SET],
    axioms: &[&COUPLING],
    invariants: &[&LEVEL_LIMIT],
};

fn main() {
    let mut lamp = Lamp { level: 0 };
    println!(
        "Exposed: {} states, {} transformer, {} axiom, {} invariant",
        SURFACE.states.len(),
        SURFACE.transformers.len(),
        SURFACE.axioms.len(),
        SURFACE.invariants.len(),
    );
    SURFACE.transformers[0]
        .invoke(
            &mut lamp,
            &[
                Value::boolean(true),
                Value::integer(Integer::U8, 128).unwrap(),
            ],
        )
        .unwrap();
    for state in SURFACE.states {
        println!(
            "Observed {:?}: {:?}",
            state.definition().ty,
            state.read(&lamp)
        );
    }
}
