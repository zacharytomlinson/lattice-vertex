//! A local delta reaches native activation code, which invokes a transformer.
//! Hardware confirmation later raises the output's own independent delta.

use lattice_vertex::{
    ir::{Effect, Expr, Value},
    observation::{Dependent, Transition},
    surface::{
        Arguments, Definition, Operation, Predicate, State, StateHandle, StateTransformer,
        TransformerHandle,
    },
};

#[derive(Default)]
struct Panel {
    contact: Option<bool>,
    output: Option<bool>,
    requested: Option<bool>,
    calls: usize,
}
struct Contact;
impl State for Contact {
    type Appliance = Panel;
    type Value = bool;
    fn read(a: &Panel) -> Option<bool> {
        a.contact
    }
}
struct Output;
impl State for Output {
    type Appliance = Panel;
    type Value = bool;
    fn read(a: &Panel) -> Option<bool> {
        a.output
    }
}
static CONTACT: StateHandle<Panel> = StateHandle::new::<Contact>();
static OUTPUT: StateHandle<Panel> = StateHandle::new::<Output>();
static PERMITTED: Definition<Panel> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: &[],
    inputs: &[],
};
struct SetOutput;
impl StateTransformer for SetOutput {
    type Appliance = Panel;
    type Arguments = (bool,);
    type Error = core::convert::Infallible;
    const DEFINITION: Operation<Panel> = Operation {
        precondition: Predicate {
            definition: &PERMITTED,
            inputs: &[],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[OUTPUT.definition()],
            results: &[Expr::Argument(0)],
            arguments: Self::Arguments::TYPES,
        },
    };
    fn invoke(a: &mut Panel, (on,): (bool,)) -> Result<(), Self::Error> {
        a.requested = Some(on);
        a.calls += 1;
        Ok(())
    }
}
static SET_OUTPUT: TransformerHandle<Panel, core::convert::Infallible> =
    TransformerHandle::new::<SetOutput>();

// These native callbacks stand in for installed, target-compiled activation
// fragments. There is no IR interpretation, synthesized code, or transport here.
fn contact_activation(a: &mut Panel, _: &StateHandle<Panel>, transition: Transition) {
    if let Some(value) = transition.after {
        SET_OUTPUT.invoke(a, &[value]).unwrap();
    }
}
fn output_activation(_: &mut Panel, _: &StateHandle<Panel>, transition: Transition) {
    println!("Output observation changed: {:?}", transition.after);
}
static CONTACT_HEAD: Dependent<Panel> = Dependent {
    activate: contact_activation,
    next: None,
};
static OUTPUT_HEAD: Dependent<Panel> = Dependent {
    activate: output_activation,
    next: None,
};

fn main() {
    let mut panel = Panel::default();
    CONTACT.set_dependents(Some(&CONTACT_HEAD));
    OUTPUT.set_dependents(Some(&OUTPUT_HEAD));
    CONTACT.update(&mut panel, |a| a.contact = Some(true));
    CONTACT.update(&mut panel, |a| a.contact = Some(true));
    assert_eq!(panel.calls, 1);
    assert_eq!(panel.output, None);
    println!("One request registered; unchanged report generated no second request.");
    OUTPUT.update(&mut panel, |a| a.output = Some(true));
}
