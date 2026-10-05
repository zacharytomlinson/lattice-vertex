use lattice_vertex::{
    ir::Value,
    observation::{self, Dependent},
    surface::{State, StateHandle},
};

#[derive(Default)]
struct Appliance {
    contact: Option<bool>,
    level: u8,
    order: [u8; 16],
    count: usize,
    last_read: Option<Value>,
}
impl Appliance {
    fn record(&mut self, entry: u8) {
        self.order[self.count] = entry;
        self.count += 1;
    }
}
struct Contact;
impl State for Contact {
    type Appliance = Appliance;
    type Value = bool;
    fn read(a: &Appliance) -> Option<bool> {
        a.contact
    }
}
struct Level;
impl State for Level {
    type Appliance = Appliance;
    type Value = u8;
    fn read(a: &Appliance) -> Option<u8> {
        Some(a.level)
    }
}

#[test]
fn observation_deltas_include_availability_and_suppress_unchanged_reports() {
    static CONTACT: StateHandle<Appliance> = StateHandle::new::<Contact>();
    fn first(a: &mut Appliance, state: &StateHandle<Appliance>) {
        assert!(core::ptr::eq(state, &CONTACT));
        a.last_read = state.read(a);
        a.record(1);
    }
    fn second(a: &mut Appliance, _: &StateHandle<Appliance>) {
        a.record(2);
    }
    static SECOND: Dependent<Appliance> = Dependent {
        activate: second,
        next: None,
    };
    static FIRST: Dependent<Appliance> = Dependent {
        activate: first,
        next: Some(&SECOND),
    };
    CONTACT.set_dependents(Some(&FIRST));
    let mut appliance = Appliance::default();
    for (value, changed) in [
        (None, false),
        (Some(false), true),
        (Some(false), false),
        (Some(true), true),
        (None, true),
        (None, false),
        (Some(false), true),
    ] {
        appliance.count = 0;
        let returned = CONTACT.update(&mut appliance, |a| {
            a.contact = value;
            73
        });
        assert_eq!(returned, 73);
        assert_eq!(
            &appliance.order[..appliance.count],
            if changed { &[1, 2][..] } else { &[] }
        );
        if changed {
            assert_eq!(appliance.last_read, value.map(Value::boolean));
        }
    }
}

#[test]
fn complete_report_is_visible_before_any_dependent_and_errors_do_not_hide_changes() {
    static CONTACT: StateHandle<Appliance> = StateHandle::new::<Contact>();
    static LEVEL: StateHandle<Appliance> = StateHandle::new::<Level>();
    fn changed(a: &mut Appliance, state: &StateHandle<Appliance>) {
        assert_eq!(a.contact, Some(true));
        if core::ptr::eq(state, &CONTACT) {
            assert_eq!(a.level, 42); // the complete original report is visible
            a.record(1);
            LEVEL.update(a, |a| a.level = 0);
        } else {
            assert_eq!(a.level, 0); // read the newer value, not captured report data
            a.record(2);
        }
    }
    static HEAD: Dependent<Appliance> = Dependent {
        activate: changed,
        next: None,
    };
    CONTACT.set_dependents(Some(&HEAD));
    LEVEL.set_dependents(Some(&HEAD));
    let mut appliance = Appliance::default();
    let result = observation::update(&mut appliance, &[&CONTACT, &LEVEL], |a| {
        a.contact = Some(true);
        a.level = 42;
        Err::<(), _>("report included a fault")
    });
    assert_eq!(result, Err("report included a fault"));
    // Level changed twice. Returning to its original value during the first
    // callback must not erase its still-valid delta from the original report.
    assert_eq!(&appliance.order[..appliance.count], &[1, 2, 2]);
}

#[test]
fn later_changes_do_not_cancel_earlier_dependents_and_reads_are_fresh() {
    static CONTACT: StateHandle<Appliance> = StateHandle::new::<Contact>();
    fn first(a: &mut Appliance, state: &StateHandle<Appliance>) {
        a.record(1);
        if a.contact == Some(true) {
            // A second established observation arrives during the first dispatch.
            state.update(a, |a| a.contact = Some(false));
        }
    }
    fn second(a: &mut Appliance, state: &StateHandle<Appliance>) {
        assert_eq!(state.read(a), Some(Value::boolean(false)));
        a.record(2);
    }
    static SECOND: Dependent<Appliance> = Dependent {
        activate: second,
        next: None,
    };
    static FIRST: Dependent<Appliance> = Dependent {
        activate: first,
        next: Some(&SECOND),
    };
    CONTACT.set_dependents(Some(&FIRST));
    let mut appliance = Appliance::default();
    CONTACT.update(&mut appliance, |a| a.contact = Some(true));
    assert_eq!(&appliance.order[..appliance.count], &[1, 1, 2, 2]);
}

#[test]
fn replacing_local_dependents_affects_future_deltas_without_withdrawing_active_work() {
    static CONTACT: StateHandle<Appliance> = StateHandle::new::<Contact>();
    fn replacement(a: &mut Appliance, _: &StateHandle<Appliance>) {
        a.record(3);
    }
    static REPLACEMENT: Dependent<Appliance> = Dependent {
        activate: replacement,
        next: None,
    };
    fn first(a: &mut Appliance, state: &StateHandle<Appliance>) {
        a.record(1);
        state.set_dependents(Some(&REPLACEMENT));
    }
    fn second(a: &mut Appliance, _: &StateHandle<Appliance>) {
        a.record(2);
    }
    static SECOND: Dependent<Appliance> = Dependent {
        activate: second,
        next: None,
    };
    static FIRST: Dependent<Appliance> = Dependent {
        activate: first,
        next: Some(&SECOND),
    };
    CONTACT.set_dependents(Some(&FIRST));
    let mut appliance = Appliance::default();
    CONTACT.update(&mut appliance, |a| a.contact = Some(true));
    CONTACT.update(&mut appliance, |a| a.contact = Some(false));
    assert_eq!(&appliance.order[..appliance.count], &[1, 2, 3]);
    CONTACT.set_dependents(None);
    CONTACT.update(&mut appliance, |a| a.contact = Some(true));
    assert_eq!(appliance.count, 3);
}

#[test]
fn explicit_hardware_changes_are_not_coalesced_and_empty_groups_still_apply_updates() {
    static CONTACT: StateHandle<Appliance> = StateHandle::new::<Contact>();
    fn changed(a: &mut Appliance, state: &StateHandle<Appliance>) {
        a.last_read = state.read(a);
        a.record(1);
    }
    static HEAD: Dependent<Appliance> = Dependent {
        activate: changed,
        next: None,
    };
    CONTACT.set_dependents(Some(&HEAD));
    let mut appliance = Appliance {
        contact: Some(true),
        ..Appliance::default()
    };
    CONTACT.changed(&mut appliance);
    CONTACT.changed(&mut appliance);
    assert_eq!(&appliance.order[..appliance.count], &[1, 1]);
    let value = observation::update(&mut appliance, &[], |a| {
        a.level = 19;
        23
    });
    assert_eq!(value, 23);
    assert_eq!(appliance.level, 19);
    assert_eq!(appliance.last_read, Some(Value::boolean(true)));
}
