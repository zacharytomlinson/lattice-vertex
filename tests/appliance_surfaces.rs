#[path = "../examples/appliances/mod.rs"]
pub mod appliances;

use appliances::{diffuser as d, radio_fan as f, shelly_dimmer as s};
use hal_protocol::{Command, Snapshot, SnapshotFlags};
use lattice_vertex::{
    ir::{Integer, Value},
    surface::InvokeError,
};
use matter_topology::{DiffuserFaults, HvacFaults};
use mhz_radio_surface::{Effect as NativeEffect, Target, DIRECTION, OFF, SPEED};
use s3_control::{Config, Controller, RequestError};
use std::cell::Cell;

thread_local! {
    static RELAY_SENT: Cell<Option<Command>> = const { Cell::new(None) };
    static FAN_SENT: Cell<Option<Target>> = const { Cell::new(None) };
    static DIMMER_SENT: Cell<Option<(bool, u8)>> = const { Cell::new(None) };
}
fn relay_send(command: Command) -> Result<(), d::Error> {
    RELAY_SENT.set(Some(command));
    Ok(())
}
fn fan_send(target: Target) -> Result<(), f::Error> {
    target.validate().unwrap();
    FAN_SENT.set(Some(target));
    Ok(())
}
fn dimmer_set(on: bool, level: u8) -> Result<(), s::Error> {
    DIMMER_SENT.set(Some((on, level)));
    Ok(())
}
fn scalar(value: u8) -> Value {
    Value::integer(Integer::U8, i128::from(value)).unwrap()
}

fn snapshot(on: bool, qualified: bool) -> Snapshot {
    let mut flags = SnapshotFlags::MASS_VALID
        | SnapshotFlags::RAW_HX711_VALID
        | SnapshotFlags::RMS_CURRENT_VALID
        | SnapshotFlags::FAN_RUNNING;
    if on {
        flags |= SnapshotFlags::RELAY_ON;
    }
    if qualified {
        flags |= SnapshotFlags::FAN_STATE_QUALIFIED;
    }
    Snapshot {
        uptime_ms: 100,
        rms_current_ma: 850,
        mass_mg: -25,
        raw_hx711: -100,
        hvac_faults: HvacFaults::from_bits_truncate(0),
        diffuser_faults: DiffuserFaults::from_bits_truncate(0),
        flags: SnapshotFlags::from_bits_truncate(flags),
    }
}
fn diffuser() -> d::Diffuser {
    d::Diffuser {
        controller: Controller::new(Config {
            relay_lease_ms: 5_000,
            relay_renewal_margin_ms: 1_000,
            relay_ack_timeout_ms: 750,
        })
        .unwrap(),
        now_ms: || 100,
        send: relay_send,
    }
}

#[test]
fn diffuser_uses_real_controller_guards_commands_and_qualified_observations() {
    let mut device = diffuser();
    assert_eq!(d::RELAY.read(&device), None);
    assert_eq!(d::MASS.read(&device), None);
    assert_eq!(d::FAN_RUNNING.read(&device), None);
    device.update_snapshot(snapshot(false, false));
    assert_eq!(d::FAN_RUNNING.read(&device), None); // stale discriminator bit is not fresh truth
    assert_eq!(
        d::MASS.read(&device),
        Some(Value::integer(Integer::I64, -25).unwrap())
    );
    device.update_snapshot(snapshot(false, true));
    assert_eq!(d::FAN_RUNNING.read(&device), Some(Value::boolean(true)));
    assert_eq!(
        d::CURRENT.read(&device),
        Some(Value::integer(Integer::U32, 850).unwrap())
    );

    for (degraded, on) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut device = diffuser();
        device.update_snapshot(snapshot(false, true));
        device.controller.set_link_degraded(100, degraded);
        RELAY_SENT.set(None);
        let result = d::SET_RELAY.invoke(&mut device, &[Value::boolean(on)]);
        if degraded && on {
            assert_eq!(
                result,
                Err(InvokeError::Appliance(d::Error::Request(
                    RequestError::LinkDegraded
                )))
            );
            assert_eq!(RELAY_SENT.get(), None);
        } else {
            assert_eq!(result, Ok(()));
            assert!(
                matches!(RELAY_SENT.get(), Some(Command::RelayLease { on: actual, lease_ms, .. }) if actual == on && lease_ms == if on { 5_000 } else { 0 })
            );
        }
        assert_eq!(d::RELAY.read(&device), Some(Value::boolean(false)));
    }
    RELAY_SENT.set(None);
    d::SET_RELAY
        .invoke(&mut device, &[Value::boolean(true)])
        .unwrap();
    let sent = RELAY_SENT.get();
    assert_eq!(
        d::SET_RELAY.invoke(&mut device, &[Value::boolean(true)]),
        Err(InvokeError::Appliance(d::Error::Request(
            RequestError::CommandPending
        )))
    );
    assert_eq!(RELAY_SENT.get(), sent);
    device.update_snapshot(snapshot(true, true));
    assert_eq!(d::RELAY.read(&device), Some(Value::boolean(true)));
}

#[test]
fn trained_fan_exposes_exact_levers_preserves_selections_and_never_infers_rf_feedback() {
    let observed = f::Observation {
        power: true,
        selected_speed: 2,
        reverse: false,
    };
    let mut fan = f::Fan {
        observed: Some(observed),
        send: fan_send,
    };
    assert_eq!(f::SURFACE.transformers.len(), 3); // Off, speed, direction; no fabricated On/light
    FAN_SENT.set(None);
    f::TURN_OFF.invoke(&mut fan, &[]).unwrap();
    assert_eq!(
        FAN_SENT.get(),
        Some(Target {
            operation: OFF,
            value: None,
            effect: NativeEffect::Set
        })
    );
    assert_eq!(fan.observed, Some(observed));
    fan.update(Some(f::Observation {
        power: false,
        ..observed
    }));
    assert_eq!(f::SELECTED_SPEED.read(&fan), Some(scalar(2)));
    assert_eq!(f::RUNNING_SPEED.read(&fan), Some(scalar(0)));

    for speed in [0, 1, 6, 7, 255] {
        FAN_SENT.set(None);
        let before = fan.observed;
        let result = f::SET_SPEED.invoke(&mut fan, &[scalar(speed)]);
        if (1..=6).contains(&speed) {
            assert_eq!(result, Ok(()));
            assert_eq!(
                FAN_SENT.get(),
                Some(Target {
                    operation: SPEED,
                    value: Some(u16::from(speed)),
                    effect: NativeEffect::Set
                })
            );
        } else {
            assert_eq!(result, Err(InvokeError::Appliance(f::Error::InvalidSpeed)));
            assert_eq!(FAN_SENT.get(), None);
        }
        assert_eq!(fan.observed, before);
    }
    FAN_SENT.set(None);
    f::SET_DIRECTION
        .invoke(&mut fan, &[Value::boolean(true)])
        .unwrap();
    assert_eq!(
        FAN_SENT.get(),
        Some(Target {
            operation: DIRECTION,
            value: Some(1),
            effect: NativeEffect::Set
        })
    );
    fan.update(None);
    FAN_SENT.set(None);
    assert_eq!(f::POWER.read(&fan), None);
    assert_eq!(
        f::SET_DIRECTION.invoke(&mut fan, &[Value::boolean(false)]),
        Err(InvokeError::Appliance(f::Error::DirectionUnknown))
    );
    assert_eq!(FAN_SENT.get(), None);
    // Absolute Off remains usable when the receiver's state is unknown.
    f::TURN_OFF.invoke(&mut fan, &[]).unwrap();
    assert_eq!(fan.observed, None);
}

#[test]
fn dimmer_keeps_output_and_brightness_independent_and_rejects_unsafe_requests() {
    for (ready, on, brightness, expected) in [
        (true, true, 0, Ok(())),
        (true, true, 100, Ok(())),
        (false, false, 75, Ok(())),
        (false, true, 75, Err(s::Error::NotReady)),
        (true, true, 101, Err(s::Error::InvalidBrightness)),
    ] {
        let observation = s::Observation {
            output: false,
            brightness: 75,
            ready,
        };
        let mut dimmer = s::Dimmer {
            observed: Some(observation),
            set: dimmer_set,
        };
        assert_eq!(s::BRIGHTNESS.read(&dimmer), Some(scalar(75)));
        assert_eq!(s::EFFECTIVE_LEVEL.read(&dimmer), Some(scalar(0)));
        DIMMER_SENT.set(None);
        let result = s::SET.invoke(&mut dimmer, &[Value::boolean(on), scalar(brightness)]);
        assert_eq!(result, expected.map_err(InvokeError::Appliance));
        assert_eq!(
            DIMMER_SENT.get(),
            if expected.is_ok() {
                Some((on, brightness))
            } else {
                None
            }
        );
        assert_eq!(dimmer.observed, Some(observation));
    }
    let mut dimmer = s::Dimmer {
        observed: None,
        set: dimmer_set,
    };
    s::SET
        .invoke(&mut dimmer, &[Value::boolean(false), scalar(0)])
        .unwrap();
    assert_eq!(s::OUTPUT.read(&dimmer), None);
}

#[test]
fn each_model_enumerates_every_declared_effect_and_predicate_state() {
    // This consumer uses only the shared IR interface, just as nucleus can.
    fn check(surface: &impl lattice_vertex::ir::Surface) {
        let owns = |attribute| {
            surface
                .states()
                .any(|state| core::ptr::eq(state, attribute))
        };
        for definition in surface.transformers() {
            assert_eq!(
                definition.effect.outputs.len(),
                definition.effect.results.len()
            );
            for state in definition
                .effect
                .outputs
                .iter()
                .chain(definition.effect.inputs)
            {
                assert!(owns(*state));
            }
            for state in definition.precondition.inputs {
                assert!(owns(*state));
            }
            assert_eq!(
                definition.precondition.inputs.len(),
                definition.precondition.definition.inputs.len()
            );
        }
        for predicate in surface.axioms().chain(surface.invariants()) {
            assert!(predicate.definition.arguments.is_empty());
            assert_eq!(predicate.inputs.len(), predicate.definition.inputs.len());
            for (state, ty) in predicate.inputs.iter().zip(predicate.definition.inputs) {
                assert!(owns(*state));
                assert_eq!(state.ty, *ty);
            }
        }
    }
    check(&d::SURFACE);
    check(&d::HVAC_SURFACE);
    check(&f::SURFACE);
    check(&s::SURFACE);
    let fan_off = f::TURN_OFF.definition();
    assert!(fan_off
        .effect
        .outputs
        .iter()
        .all(|state| !core::ptr::eq(*state, f::SELECTED_SPEED.definition())));
    assert_eq!(f::SET_SPEED.definition().effect.outputs.len(), 3);
    assert_eq!(s::SET.definition().effect.outputs.len(), 3);
}
