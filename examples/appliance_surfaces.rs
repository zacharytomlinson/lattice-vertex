//! Inspect the appliance models without invoking or contacting hardware.
pub mod appliances;

use lattice_vertex::ir::Surface;

fn describe(name: &str, surface: &impl Surface) {
    println!(
        "{name}: {} states, {} transformers, {} axioms, {} invariants",
        surface.states().count(),
        surface.transformers().count(),
        surface.axioms().count(),
        surface.invariants().count()
    );
    for transformer in surface.transformers() {
        let effect = &transformer.effect;
        println!(
            "  {:?} -> {} coupled state effects",
            effect.arguments,
            effect.outputs.len()
        );
    }
}

fn main() {
    describe("Diffuser", &appliances::diffuser::SURFACE);
    describe("HVAC observer", &appliances::diffuser::HVAC_SURFACE);
    describe("Trained RC400 fan", &appliances::radio_fan::SURFACE);
    describe(
        "Shelly dimmer proposal",
        &appliances::shelly_dimmer::SURFACE,
    );
}
