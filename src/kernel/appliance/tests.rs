//! State binding, captured ordering, capacity and native-call boundaries.
extern crate std;
use super::*;
use crate::{
    kernel::{State, StateChangeError, StateEvent},
    surface::{Scalar, StateHandle, StateTransformer, TransformerHandle},
};
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering::Relaxed};
struct Device {
    value: State<bool>,
    edge: StateEvent<bool>,
    pin: AtomicU8,
    reads: [AtomicU32; 2],
}
impl Device {
    fn new(values: [Option<bool>; 2]) -> Self {
        Self {
            value: State::restore(values[0]),
            edge: StateEvent::new(),
            pin: AtomicU8::new(values[1].map_or(0, |v| 1 + u8::from(v))),
            reads: [const { AtomicU32::new(0) }; 2],
        }
    }
    fn set(&self, value: bool) -> Result<(), ()> {
        if value {
            return Ok(());
        }
        self.value.set(false).map_err(|_| ())?;
        Err(())
    }
    fn reads(&self) -> [u32; 2] {
        self.reads.each_ref().map(|v| v.load(Relaxed))
    }
}
fn read_pin(a: &Device) -> Option<bool> {
    a.reads[1].fetch_add(1, Relaxed);
    match a.pin.load(Relaxed) {
        0 => None,
        n => Some(n == 2),
    }
}
static A: StateHandle<Device> = StateHandle::new("a", |a| &a.value);
static B: StateHandle<Device> = StateHandle::hardware("b", read_pin, |a| &a.edge);
struct Set;
impl StateTransformer for Set {
    type Appliance = Device;
    type Arguments = (bool,);
    type Error = ();

    fn invoke(a: &'static Device, (v,): (bool,)) -> Result<(), ()> {
        a.set(v)
    }
}
static SET: TransformerHandle<Device, ()> = TransformerHandle::new::<Set>("set");
static DIRECT: TransformerHandle<Device, ()> = TransformerHandle::from_fn("direct", Device::set);
static SURFACE: Surface<Device, ()> = Surface {
    states: &[&A, &B],
    transformers: &[&SET, &DIRECT],
    model: None,
};
fn initialize() -> (TaskContext, &'static Appliance<Device, ()>) {
    crate::port::test_port_reset();
    super::super::change::reset();
    let context = unsafe { TaskContext::assume() };
    let owner = std::boxed::Box::leak(std::boxed::Box::new(Appliance::new(&SURFACE)));
    owner.initialize(&context, Device::new([None, None]));
    super::super::change::bind(owner);
    (context, owner)
}

#[test]
fn declaration_metadata() {
    use crate::kernel::peer::exposure::Description;

    static FIRST: Appliance<Device, ()> = crate::appliance! {
        name: concat!("First", " appliance"),
        surface: &SURFACE,
    };
    static SECOND: Appliance<Device, ()> = crate::appliance! {
        name: "Dining room — appliance with a descriptive name that spans more than one radio frame",
        surface: &SURFACE,
    };

    crate::port::test_port_reset();
    let context = unsafe { TaskContext::assume() };
    let expected = Description::new(&SURFACE).unwrap().identity();
    for (owner, name) in [
        (&FIRST, "First appliance"),
        (
            &SECOND,
            "Dining room — appliance with a descriptive name that spans more than one radio frame",
        ),
    ] {
        owner.initialize(&context, Device::new([Some(false), None]));
        assert_eq!(owner.name(), Some(name));
        let description = Description::with_installed(owner).unwrap();
        assert_eq!(description.identity(), expected);
        let mut bytes = [0; 256];
        let length = description.len() as usize;
        for width in [1, 7, lattice_peer_wire::SURFACE_BYTES] {
            for (index, chunk) in bytes[..length].chunks_mut(width).enumerate() {
                assert_eq!(
                    description.encode_part((index * width) as u16, chunk),
                    chunk.len()
                );
            }
            let (_, received_name) = lattice_peer_wire::surface_parts(&bytes[..length]).unwrap();
            assert_eq!(received_name, Some(name));
        }
        assert_eq!(owner.device().value.get(), Some(false));
    }
    assert_eq!(Appliance::new(&SURFACE).name(), None);
    for invalid in ["", "invalid\0name"] {
        assert!(std::panic::catch_unwind(|| Appliance::new(&SURFACE).with_name(invalid)).is_err());
    }
}

#[test]
fn state_value_and_registration_bounds() {
    for (initial, result) in [
        (None, Err(StateChangeError::Unavailable)),
        (Some(false), Ok(true)),
        (Some(true), Ok(false)),
    ] {
        let state = State::restore(initial);
        assert_eq!(state.toggle(), result);
        assert_eq!(state.get(), result.ok().or(initial));
    }
    static ALIAS: StateHandle<Device> = StateHandle::new("alias", |a| &a.value);
    static DUPLICATED: Surface<Device, ()> = Surface::new(&[&A, &ALIAS], &[]);
    crate::port::test_port_reset();
    let context = unsafe { TaskContext::assume() };
    let owner = std::boxed::Box::leak(std::boxed::Box::new(Appliance::new(&DUPLICATED)));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || owner.initialize(&context, Device::new([None, None]))
    ))
    .is_err());
}

#[test]
fn captured_transitions() {
    let (context, owner) = initialize();
    let a = &owner.device().value;
    let mut expected = std::vec::Vec::new();
    for (value, changed) in [
        (Some(true), true),
        (Some(true), false),
        (Some(false), true),
        (None, true),
        (None, false),
    ] {
        assert_eq!(a.replace(value), Ok(changed));
        if changed {
            expected.push(value.map(Scalar::into_value));
        }
    }
    // Drain only after the source has changed repeatedly. Values must be captured,
    // not reread at dispatch time or coalesced into a single readiness bit.
    assert_eq!(owner.read(A.reference()), StateReadResult::Busy);
    owner.process_changes(&context);
    let actual: std::vec::Vec<_> = core::iter::from_fn(super::super::change::take)
        .map(|c| c.value)
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(owner.device().reads(), [0, 0]);
    assert!(!owner.events.pending());
    assert_eq!(owner.state_change_overflow(), 0);
    super::super::change::reset();
}
#[test]
fn hardware_event_values() {
    let (context, owner) = initialize();
    let unbound = StateEvent::<bool>::new();
    let mut captured = false;
    assert_eq!(
        unbound.handle(|| {
            captured = true;
            Some(true)
        }),
        Err(StateChangeError::NotReady)
    );
    assert!(captured);

    crate::port::test_port_set_inside_interrupt(true);
    for value in [Some(true), Some(true), None, Some(false)] {
        owner
            .device()
            .edge
            .handle(|| {
                owner
                    .device()
                    .pin
                    .store(value.map_or(0, |v| 1 + u8::from(v)), Relaxed);
                value
            })
            .unwrap();
    }
    crate::port::test_port_set_inside_interrupt(false);
    owner.process_changes(&context);
    for expected in [Some(true), Some(true), None, Some(false)] {
        let c = super::super::change::take().unwrap();
        assert_eq!(
            (c.attribute, c.value),
            (B.reference(), expected.map(Scalar::into_value))
        );
    }
    assert!(super::super::change::take().is_none());
    assert_eq!(owner.device().reads(), [0, 0]);
    super::super::change::reset();
}
#[test]
fn transition_capacity() {
    let (context, owner) = initialize();
    for _ in 0..16 {
        super::super::work::defer(super::super::work::Priority::Background, &(), |_| {});
    }
    let input = &owner.device().edge;
    for i in 0..crate::configLATTICE_STATE_CHANGES {
        input.handle(|| Some(i % 2 != 0)).unwrap();
    }
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        owner.device().value.set(true).unwrap();
    }))
    .is_err());
    // Overflow is a kernel fault after the immediate mutation, never a rejected
    // operation that firmware is asked to perform again.
    assert_eq!(owner.device().value.get(), Some(true));
    assert_eq!(owner.state_change_overflow(), 1);
    for i in 0..crate::configLATTICE_STATE_CHANGES {
        let c = owner.events.take(&context).unwrap();
        assert_eq!((c.index, c.value()), (1, Some(Value::boolean(i % 2 != 0))));
    }
    assert!(owner.events.take(&context).is_none());
    assert_eq!(owner.device().value.set(false), Ok(true));
    owner.process_changes(&context);
    assert_eq!(
        super::super::change::take().unwrap().value,
        Some(Value::boolean(false))
    );
    super::super::change::reset();
}
#[test]
fn native_admission() {
    let (context, owner) = initialize();
    assert_eq!(SURFACE.ir_status(), lattice_peer_wire::IrStatus::Absent);
    for callable in [&SET, &DIRECT] {
        for (handle, args, status) in [
            (
                0,
                &[Value::boolean(true)][..],
                InvokeStatus::UnknownTransformer,
            ),
            (
                ptr::from_ref(&SET) as usize as u64,
                &[Value::boolean(true)][..],
                InvokeStatus::UnknownTransformer,
            ),
            (callable.reference(), &[][..], InvokeStatus::Arguments),
            (
                callable.reference(),
                &[Value::integer(crate::ir::Integer::U8, 1).unwrap()][..],
                InvokeStatus::Arguments,
            ),
            (
                callable.reference(),
                &[Value::boolean(true)][..],
                InvokeStatus::Invoked,
            ),
            (
                callable.reference(),
                &[Value::boolean(false)][..],
                InvokeStatus::ApplianceError,
            ),
        ] {
            assert_eq!(owner.invoke(handle, args), status);
        }
    }
    owner.process_changes(&context);
    assert_eq!(
        super::super::change::take().unwrap().value,
        Some(Value::boolean(false))
    );
    assert!(super::super::change::take().is_none());
    assert_eq!(owner.device().reads(), [0, 0]);
    // Firmware continues changing state after the callable has returned.
    owner.device().value.set(true).unwrap();
    owner.process_changes(&context);
    assert_eq!(
        super::super::change::take().unwrap().value,
        Some(Value::boolean(true))
    );
    assert_eq!(
        owner.call(|a| a.set(false)),
        Err(InvokeError::Appliance(()))
    );
    owner.process_changes(&context);
    assert_eq!(
        super::super::change::take().unwrap().value,
        Some(Value::boolean(false))
    );
    assert_eq!(
        owner.read(SET.reference()),
        StateReadResult::UnknownAttribute
    );
    super::super::change::reset();
}
#[test]
fn binding_resolution() {
    let mut reference = None;
    for index in [0, 1] {
        let handle = std::boxed::Box::leak(std::boxed::Box::new(if index == 0 {
            StateHandle::new("input", |a: &Device| &a.value)
        } else {
            StateHandle::hardware("input", read_pin, |a: &Device| &a.edge)
        }));
        let surface = std::boxed::Box::leak(std::boxed::Box::new(Surface::<Device, ()> {
            states: std::boxed::Box::leak(std::boxed::Box::new([&*handle])),
            transformers: &[],
            model: Some(&crate::surface::IrModel {
                complete: true,
                axioms: &[],
                invariants: &[],
            }),
        }));
        crate::port::test_port_reset();
        let context = unsafe { TaskContext::assume() };
        let owner = std::boxed::Box::leak(std::boxed::Box::new(Appliance::new(surface)));
        owner.initialize(&context, Device::new([Some(true), Some(false)]));
        let saved = *reference.get_or_insert(handle.reference());
        assert_eq!(saved, handle.reference());
        assert!(
            matches!(owner.read(saved),StateReadResult::Value{value:Some(v),..} if v==Value::boolean(index==0))
        );
        assert_eq!(
            owner.read(ptr::from_ref(handle.definition()) as usize as u64),
            StateReadResult::UnknownAttribute
        );
    }
}

#[test]
fn private_setting_policy() {
    static SETTING: StateHandle<Device> =
        StateHandle::new("setting", |d: &Device| &d.value).nonvolatile();
    static DECLARATION: Surface<Device, ()> = Surface {
        states: &[],
        transformers: &[],
        model: None,
    };
    crate::port::test_port_reset();
    super::super::change::reset();
    let context = unsafe { TaskContext::assume() };
    static SETTINGS: [&StateHandle<Device>; 1] = [&SETTING];
    let owner = std::boxed::Box::leak(std::boxed::Box::new(
        Appliance::new(&DECLARATION).with_settings(&SETTINGS),
    ));
    owner.initialize(&context, Device::new([Some(false), None]));
    crate::kernel::storage::take_settings();
    owner
        .restore_setting(
            SETTING.reference(),
            crate::ir::Type::Bool,
            Some(Value::boolean(true)),
        )
        .unwrap();
    assert_eq!(owner.device().value.get(), Some(true));
    assert!(!owner.events.pending());
    assert!(!crate::kernel::storage::settings_pending());
    owner.device().value.set(false).unwrap();
    let now = ((crate::kernel::tasks::xTaskGetTickCountFromISR() as u64 * 1000)
        / crate::configTICK_RATE_HZ as u64) as u32;
    for (elapsed, due) in [(0, false), (999, false), (1000, true)] {
        assert_eq!(
            crate::kernel::storage::take_quiet_settings(now.wrapping_add(elapsed)),
            due
        );
    }
    assert!(!crate::kernel::storage::settings_pending());
    assert!(!owner.events.pending());
    let mut values = 0;
    owner.settings(&mut |key, ty, value| {
        assert_eq!(
            (key, ty, value),
            (
                SETTING.reference(),
                crate::ir::Type::Bool,
                Some(Value::boolean(false))
            )
        );
        values += 1;
    });
    assert_eq!(values, 1);
    assert_eq!(owner.surface.states.len(), 0);
}
