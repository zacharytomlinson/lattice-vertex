//! Proposed minimal custom-firmware surface for Shelly Dimmer Gen4 hardware.
//! Uses the documented independent output and 0..100 brightness controls.
//! Calibration, warmup, protection, and hardware I/O stay in the appliance backend.

use lattice_vertex::{
    ir::{Binary, Effect, Expr, Integer},
    surface::{
        Arguments, Definition, Operation, Predicate, StateTransformer, Surface, TransformerHandle,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidBrightness,
    NotReady,
    Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub output: bool,
    pub brightness: u8,
    /// Backend-established readiness: calibration/protection permit energizing.
    pub ready: bool,
}
pub struct Dimmer {
    pub observed: Option<Observation>,
    /// Request both values in one native operation. Does not publish observed state.
    pub set: fn(bool, u8) -> Result<(), Error>,
}
impl Dimmer {
    /// Register a complete established status before any dependent executes.
    pub fn update(&mut self, observed: Option<Observation>) {
        lattice_vertex::observation::update(
            self,
            &[&OUTPUT, &BRIGHTNESS, &READY, &EFFECTIVE_LEVEL],
            |a| a.observed = observed,
        );
    }
}
state!(Output, OUTPUT, Dimmer, bool, |a: &Dimmer| a
    .observed
    .map(|s| s.output));
state!(Brightness, BRIGHTNESS, Dimmer, u8, |a: &Dimmer| a
    .observed
    .map(|s| s.brightness));
state!(Ready, READY, Dimmer, bool, |a: &Dimmer| a
    .observed
    .map(|s| s.ready));
state!(EffectiveLevel, EFFECTIVE_LEVEL, Dimmer, u8, |a: &Dimmer| a
    .observed
    .map(|s| if s.output { s.brightness } else { 0 }));
const ZERO: lattice_vertex::ir::Value = super::integer(Integer::U8, 0);
const HUNDRED: lattice_vertex::ir::Value = super::integer(Integer::U8, 100);
static PERMITTED: Definition<Dimmer> = Definition {
    body: Expr::And(
        &Expr::Binary(
            Binary::LessEqual,
            &Expr::Argument(1),
            &Expr::Constant(HUNDRED),
        ),
        &Expr::Or(&Expr::Not(&Expr::Argument(0)), &Expr::Input(0)),
    ),
    arguments: <(bool, u8) as Arguments>::TYPES,
    inputs: <(bool,) as Arguments>::TYPES,
};
pub struct Set;
impl StateTransformer for Set {
    type Appliance = Dimmer;
    type Arguments = (bool, u8);
    type Error = Error;
    const DEFINITION: Operation<Dimmer> = Operation {
        precondition: Predicate {
            definition: &PERMITTED,
            inputs: &[READY.definition()],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[
                OUTPUT.definition(),
                BRIGHTNESS.definition(),
                EFFECTIVE_LEVEL.definition(),
            ],
            results: &[
                Expr::Argument(0),
                Expr::Argument(1),
                Expr::Select(
                    &Expr::Argument(0),
                    &Expr::Argument(1),
                    &Expr::Constant(ZERO),
                ),
            ],
            arguments: Self::Arguments::TYPES,
        },
    };
    fn invoke(a: &mut Dimmer, (output, brightness): (bool, u8)) -> Result<(), Error> {
        if brightness > 100 {
            return Err(Error::InvalidBrightness);
        }
        if output && !a.observed.is_some_and(|s| s.ready) {
            return Err(Error::NotReady);
        }
        (a.set)(output, brightness)
    }
}
static EFFECTIVE_RELATION: Definition<Dimmer> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Input(2),
        &Expr::Select(&Expr::Input(0), &Expr::Input(1), &Expr::Constant(ZERO)),
    ),
    arguments: &[],
    inputs: <(bool, u8, u8) as Arguments>::TYPES,
};
static AXIOM: Predicate<Dimmer> = Predicate {
    definition: &EFFECTIVE_RELATION,
    inputs: &[
        OUTPUT.definition(),
        BRIGHTNESS.definition(),
        EFFECTIVE_LEVEL.definition(),
    ],
};
static BRIGHTNESS_DOMAIN: Definition<Dimmer> = Definition {
    body: Expr::Binary(Binary::LessEqual, &Expr::Input(0), &Expr::Constant(HUNDRED)),
    arguments: &[],
    inputs: <(u8,) as Arguments>::TYPES,
};
static INVARIANT: Predicate<Dimmer> = Predicate {
    definition: &BRIGHTNESS_DOMAIN,
    inputs: &[BRIGHTNESS.definition()],
};
pub static SET: TransformerHandle<Dimmer, Error> = TransformerHandle::new::<Set>();
pub static SURFACE: Surface<'static, Dimmer, Error> = Surface {
    states: &[&OUTPUT, &BRIGHTNESS, &READY, &EFFECTIVE_LEVEL],
    transformers: &[&SET],
    axioms: &[&AXIOM],
    invariants: &[&INVARIANT],
};
