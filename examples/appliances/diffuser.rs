//! Views of diffuser-fw's actual S3 controller and RA4 observations.
//! The two roots correspond to its diffuser and HVAC-observer appliance surfaces.

use hal_protocol::{Command, Snapshot};
use lattice_vertex::{
    ir::{Binary, Effect, Expr, Integer, Value},
    surface::{
        Arguments, Definition, Operation, Predicate, StateTransformer, Surface, TransformerHandle,
    },
};
use matter_topology::{DiffuserFaults, HvacFaults};
use s3_control::{Controller, RequestError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Request(RequestError),
    Transport,
}

/// Uses the existing controller rather than a second copy of its physical state.
/// `send` writes the native RA4 command; it does not publish an observation.
pub struct Diffuser {
    pub controller: Controller,
    pub now_ms: fn() -> u32,
    pub send: fn(Command) -> Result<(), Error>,
}

impl Diffuser {
    /// Apply a complete RA4 report before activating any affected dependencies.
    pub fn update_snapshot(&mut self, snapshot: Snapshot) -> s3_control::PhysicalState {
        lattice_vertex::observation::update(
            self,
            &[
                &RELAY,
                &MASS,
                &RAW_SCALE,
                &FAULTS,
                &FAN_RUNNING,
                &CURRENT,
                &HVAC_ERRORS,
            ],
            |a| a.controller.handle_snapshot(snapshot),
        )
    }
}

state!(Relay, RELAY, Diffuser, bool, |a: &Diffuser| a
    .controller
    .physical()
    .map(|p| p.relay_on));
state!(Mass, MASS, Diffuser, i64, |a: &Diffuser| a
    .controller
    .physical()
    .and_then(|p| p.mass_mg));
state!(RawScale, RAW_SCALE, Diffuser, i32, |a: &Diffuser| a
    .controller
    .physical()
    .and_then(|p| p.raw_hx711));
state!(Faults, FAULTS, Diffuser, u32, |a: &Diffuser| Some(
    a.controller.active_diffuser_faults().bits()
));
state!(FanRunning, FAN_RUNNING, Diffuser, bool, |a: &Diffuser| a
    .controller
    .physical()
    .and_then(|p| p.fan_state_qualified.then_some(p.fan_running)));
state!(Current, CURRENT, Diffuser, u32, |a: &Diffuser| a
    .controller
    .physical()
    .and_then(|p| p.rms_current_ma));
state!(HvacErrors, HVAC_ERRORS, Diffuser, u32, |a: &Diffuser| a
    .controller
    .physical()
    .map(|p| p.hvac_faults.bits()));

const ZERO: Value = super::integer(Integer::U32, 0);
const LINK_FAULT: Value =
    super::integer(Integer::U32, DiffuserFaults::RA4_S3_LINK_DEGRADED as i128);
const UNKNOWN_DIFFUSER_FAULTS: Value =
    super::integer(Integer::U32, (!DiffuserFaults::ASSIGNED_MASK) as i128);
const UNKNOWN_HVAC_FAULTS: Value =
    super::integer(Integer::U32, (!HvacFaults::ASSIGNED_MASK) as i128);

// Native safety-relevant admission: a degraded link must not accept a new On.
// Off is permitted without inspecting the link. Busy/I/O failures stay fallible.
static PERMITTED: Definition<Diffuser> = Definition {
    body: Expr::Or(
        &Expr::Not(&Expr::Argument(0)),
        &Expr::Binary(
            Binary::Equal,
            &Expr::Binary(Binary::BitAnd, &Expr::Input(0), &Expr::Constant(LINK_FAULT)),
            &Expr::Constant(ZERO),
        ),
    ),
    arguments: <(bool,) as Arguments>::TYPES,
    inputs: <(u32,) as Arguments>::TYPES,
};

pub struct SetRelay;
impl StateTransformer for SetRelay {
    type Appliance = Diffuser;
    type Arguments = (bool,);
    type Error = Error;
    const DEFINITION: Operation<Diffuser> = Operation {
        precondition: Predicate {
            definition: &PERMITTED,
            inputs: &[FAULTS.definition()],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[RELAY.definition()],
            results: &[Expr::Argument(0)],
            arguments: Self::Arguments::TYPES,
        },
    };
    fn invoke(a: &mut Diffuser, (on,): (bool,)) -> Result<(), Error> {
        // Check the exposed condition as well as the controller's native guards.
        if on
            && a.controller
                .active_diffuser_faults()
                .contains(DiffuserFaults::RA4_S3_LINK_DEGRADED)
        {
            return Err(Error::Request(RequestError::LinkDegraded));
        }
        let command = a
            .controller
            .request((a.now_ms)(), on)
            .map_err(Error::Request)?;
        (a.send)(command)
    }
}

// HX711 acquisition sign-extends a 24-bit sample into i32; averaging stays in range.
static RAW_DOMAIN: Definition<Diffuser> = Definition {
    body: Expr::And(
        &Expr::Binary(
            Binary::GreaterEqual,
            &Expr::Input(0),
            &Expr::Constant(super::integer(Integer::I32, -8_388_608)),
        ),
        &Expr::Binary(
            Binary::LessEqual,
            &Expr::Input(0),
            &Expr::Constant(super::integer(Integer::I32, 8_388_607)),
        ),
    ),
    arguments: &[],
    inputs: <(i32,) as Arguments>::TYPES,
};
static SCALE_AXIOM: Predicate<Diffuser> = Predicate {
    definition: &RAW_DOMAIN,
    inputs: &[RAW_SCALE.definition()],
};
static FAULT_DOMAIN: Definition<Diffuser> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Binary(
            Binary::BitAnd,
            &Expr::Input(0),
            &Expr::Constant(UNKNOWN_DIFFUSER_FAULTS),
        ),
        &Expr::Constant(ZERO),
    ),
    arguments: &[],
    inputs: <(u32,) as Arguments>::TYPES,
};
static FAULT_INVARIANT: Predicate<Diffuser> = Predicate {
    definition: &FAULT_DOMAIN,
    inputs: &[FAULTS.definition()],
};
static HVAC_FAULT_DOMAIN: Definition<Diffuser> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Binary(
            Binary::BitAnd,
            &Expr::Input(0),
            &Expr::Constant(UNKNOWN_HVAC_FAULTS),
        ),
        &Expr::Constant(ZERO),
    ),
    arguments: &[],
    inputs: <(u32,) as Arguments>::TYPES,
};
static HVAC_INVARIANT: Predicate<Diffuser> = Predicate {
    definition: &HVAC_FAULT_DOMAIN,
    inputs: &[HVAC_ERRORS.definition()],
};

pub static SET_RELAY: TransformerHandle<Diffuser, Error> = TransformerHandle::new::<SetRelay>();
pub static SURFACE: Surface<'static, Diffuser, Error> = Surface {
    states: &[&RELAY, &MASS, &RAW_SCALE, &FAULTS],
    transformers: &[&SET_RELAY],
    axioms: &[&SCALE_AXIOM],
    invariants: &[&FAULT_INVARIANT],
};
pub static HVAC_SURFACE: Surface<'static, Diffuser, Error> = Surface {
    states: &[&FAN_RUNNING, &CURRENT, &HVAC_ERRORS],
    transformers: &[],
    axioms: &[],
    invariants: &[&HVAC_INVARIANT],
};
