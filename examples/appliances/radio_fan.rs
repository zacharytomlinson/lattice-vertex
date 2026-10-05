//! The trained RC400 fan profile: Off, speeds 1..6, and direction; no invented On/light.
//! Native targets retain mhz-radio-fw's operation/value/effect representation.

use lattice_vertex::{
    ir::{Binary, Effect, Expr, Integer, Value},
    surface::{
        Arguments, Definition, Operation, Predicate, StateTransformer, Surface, TransformerHandle,
    },
};
use mhz_radio_surface::{Effect as NativeEffect, Target, DIRECTION, OFF, SPEED};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSpeed,
    DirectionUnknown,
    Transport,
}

/// Established observations, supplied by a real observer or explicit confirmation.
/// The release firmware's command-tracked model is not automatically RF feedback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub power: bool,
    pub selected_speed: u8,
    pub reverse: bool,
}
pub struct Fan {
    pub observed: Option<Observation>,
    pub send: fn(Target) -> Result<(), Error>,
}
impl Fan {
    /// Register established receiver observations or their loss, never a TX prediction.
    pub fn update(&mut self, observed: Option<Observation>) {
        lattice_vertex::observation::update(
            self,
            &[
                &POWER,
                &SELECTED_SPEED,
                &RUNNING_SPEED,
                &REVERSE,
                &DIRECTION_KNOWN,
            ],
            |a| a.observed = observed,
        );
    }
}
state!(Power, POWER, Fan, bool, |a: &Fan| a
    .observed
    .map(|s| s.power));
state!(SelectedSpeed, SELECTED_SPEED, Fan, u8, |a: &Fan| a
    .observed
    .map(|s| s.selected_speed));
state!(RunningSpeed, RUNNING_SPEED, Fan, u8, |a: &Fan| a
    .observed
    .map(|s| if s.power { s.selected_speed } else { 0 }));
state!(Reverse, REVERSE, Fan, bool, |a: &Fan| a
    .observed
    .map(|s| s.reverse));
state!(DirectionKnown, DIRECTION_KNOWN, Fan, bool, |a: &Fan| Some(
    a.observed.is_some()
));

const ZERO: Value = super::integer(Integer::U8, 0);
const ONE: Value = super::integer(Integer::U8, 1);
const SIX: Value = super::integer(Integer::U8, 6);
static ALWAYS: Definition<Fan> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: &[],
    inputs: &[],
};
static SPEED_ALLOWED: Definition<Fan> = Definition {
    body: Expr::And(
        &Expr::Binary(
            Binary::GreaterEqual,
            &Expr::Argument(0),
            &Expr::Constant(ONE),
        ),
        &Expr::Binary(Binary::LessEqual, &Expr::Argument(0), &Expr::Constant(SIX)),
    ),
    arguments: <(u8,) as Arguments>::TYPES,
    inputs: &[],
};
static DIRECTION_ALLOWED: Definition<Fan> = Definition {
    body: Expr::Input(0),
    arguments: <(bool,) as Arguments>::TYPES,
    inputs: <(bool,) as Arguments>::TYPES,
};

pub struct Off;
impl StateTransformer for Off {
    type Appliance = Fan;
    type Arguments = ();
    type Error = Error;
    const DEFINITION: Operation<Fan> = Operation {
        precondition: Predicate {
            definition: &ALWAYS,
            inputs: &[],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[POWER.definition(), RUNNING_SPEED.definition()],
            results: &[Expr::Constant(Value::boolean(false)), Expr::Constant(ZERO)],
            arguments: &[],
        },
    };
    fn invoke(a: &mut Fan, _: ()) -> Result<(), Error> {
        (a.send)(Target {
            operation: OFF,
            value: None,
            effect: NativeEffect::Set,
        })
    }
}
pub struct SetSpeed;
impl StateTransformer for SetSpeed {
    type Appliance = Fan;
    type Arguments = (u8,);
    type Error = Error;
    const DEFINITION: Operation<Fan> = Operation {
        precondition: Predicate {
            definition: &SPEED_ALLOWED,
            inputs: &[],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[
                POWER.definition(),
                SELECTED_SPEED.definition(),
                RUNNING_SPEED.definition(),
            ],
            results: &[
                Expr::Constant(Value::boolean(true)),
                Expr::Argument(0),
                Expr::Argument(0),
            ],
            arguments: Self::Arguments::TYPES,
        },
    };
    fn invoke(a: &mut Fan, (speed,): (u8,)) -> Result<(), Error> {
        if !(1..=6).contains(&speed) {
            return Err(Error::InvalidSpeed);
        }
        (a.send)(Target {
            operation: SPEED,
            value: Some(u16::from(speed)),
            effect: NativeEffect::Set,
        })
    }
}
pub struct SetDirection;
impl StateTransformer for SetDirection {
    type Appliance = Fan;
    type Arguments = (bool,);
    type Error = Error;
    const DEFINITION: Operation<Fan> = Operation {
        precondition: Predicate {
            definition: &DIRECTION_ALLOWED,
            inputs: &[DIRECTION_KNOWN.definition()],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[REVERSE.definition()],
            results: &[Expr::Argument(0)],
            arguments: Self::Arguments::TYPES,
        },
    };
    fn invoke(a: &mut Fan, (reverse,): (bool,)) -> Result<(), Error> {
        if a.observed.is_none() {
            return Err(Error::DirectionUnknown);
        }
        // The existing native planner selects a trained absolute waveform or
        // performs the necessary toggle, including its already-matching no-op.
        (a.send)(Target {
            operation: DIRECTION,
            value: Some(u16::from(reverse)),
            effect: NativeEffect::Set,
        })
    }
}
static SPEED_RELATION: Definition<Fan> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Input(2),
        &Expr::Select(&Expr::Input(0), &Expr::Input(1), &Expr::Constant(ZERO)),
    ),
    arguments: &[],
    inputs: <(bool, u8, u8) as Arguments>::TYPES,
};
static AXIOM: Predicate<Fan> = Predicate {
    definition: &SPEED_RELATION,
    inputs: &[
        POWER.definition(),
        SELECTED_SPEED.definition(),
        RUNNING_SPEED.definition(),
    ],
};
static SPEED_DOMAIN: Definition<Fan> = Definition {
    body: Expr::And(
        &Expr::Binary(Binary::GreaterEqual, &Expr::Input(0), &Expr::Constant(ONE)),
        &Expr::Binary(Binary::LessEqual, &Expr::Input(0), &Expr::Constant(SIX)),
    ),
    arguments: &[],
    inputs: <(u8,) as Arguments>::TYPES,
};
static INVARIANT: Predicate<Fan> = Predicate {
    definition: &SPEED_DOMAIN,
    inputs: &[SELECTED_SPEED.definition()],
};
pub static TURN_OFF: TransformerHandle<Fan, Error> = TransformerHandle::new::<Off>();
pub static SET_SPEED: TransformerHandle<Fan, Error> = TransformerHandle::new::<SetSpeed>();
pub static SET_DIRECTION: TransformerHandle<Fan, Error> = TransformerHandle::new::<SetDirection>();
pub static SURFACE: Surface<'static, Fan, Error> = Surface {
    states: &[
        &POWER,
        &SELECTED_SPEED,
        &RUNNING_SPEED,
        &REVERSE,
        &DIRECTION_KNOWN,
    ],
    transformers: &[&TURN_OFF, &SET_SPEED, &SET_DIRECTION],
    axioms: &[&AXIOM],
    invariants: &[&INVARIANT],
};
