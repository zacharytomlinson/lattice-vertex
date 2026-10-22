//! Two authored test appliances and passive surface-read diagnostics.
//! Only the kernel emits packets. Each record describes one range, not a
//! completed surface transfer; no foreign mathematical surface is retained.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering::Relaxed};
use lattice_vertex::{
    ir::{Binary, Effect, Expr, Integer, Unary, Value},
    kernel::radio::testing::Event,
    kernel::state::{History, RetainedState},
    surface::{
        Arguments, Definition, Operation, Predicate, State, StateHandle, StateTransformer, Surface,
        TransformerHandle,
    },
};

pub struct Controls {
    power: AtomicBool,
    level: AtomicU8,
}
struct Power;
impl State for Power {
    type Appliance = Controls;
    type Value = bool;
    fn read(a: &Controls) -> Option<bool> {
        Some(a.power.load(Relaxed))
    }
}
struct Level;
impl State for Level {
    type Appliance = Controls;
    type Value = u8;
    fn read(a: &Controls) -> Option<u8> {
        Some(a.level.load(Relaxed))
    }
}
static POWER: StateHandle<Controls> = StateHandle::new::<Power>();
static LEVEL: StateHandle<Controls> = StateHandle::new::<Level>();
// Native fixture history: the adapter only changes hardware-backed values.
// Kernel acquisition owns detection, history maintenance and delta publication.
static LEVEL_HISTORY: RetainedState<Controls, u8, i16, 4> = RetainedState::new(&LEVEL);
static LEVEL_DIFFERENCE: Definition<Controls> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Read(LEVEL_HISTORY.result()),
        &Expr::Binary(
            Binary::Subtract,
            &Expr::Unary(
                Unary::Reinterpret(Integer::I16),
                &Expr::Unary(
                    Unary::ZeroExtend(Integer::U16),
                    &Expr::Read(LEVEL_HISTORY.sample(0)),
                ),
            ),
            &Expr::Unary(
                Unary::Reinterpret(Integer::I16),
                &Expr::Unary(
                    Unary::ZeroExtend(Integer::U16),
                    &Expr::Read(LEVEL_HISTORY.sample(1)),
                ),
            ),
        ),
    ),
    arguments: &[],
    inputs: &[],
};
static LEVEL_DIFFERENCE_RULE: Predicate<Controls> = Predicate {
    definition: &LEVEL_DIFFERENCE,
    inputs: &[],
};
fn level_difference(history: &History<u8, 4>) -> Option<i16> {
    Some(i16::from(history.value(0)?) - i16::from(history.value(1)?))
}
const ZERO: Value = match Value::integer(Integer::U8, 0) {
    Ok(v) => v,
    Err(_) => panic!(),
};
const LIMIT: Value = match Value::integer(Integer::U8, 200) {
    Ok(v) => v,
    Err(_) => panic!(),
};
static TRUE: Definition<Controls> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: &[],
    inputs: &[],
};
static SAFE: Definition<Controls> = Definition {
    body: Expr::Binary(Binary::LessEqual, &Expr::Input(0), &Expr::Constant(LIMIT)),
    arguments: &[],
    inputs: <(u8,) as Arguments>::TYPES,
};
static SAFE_LEVEL: Predicate<Controls> = Predicate {
    definition: &SAFE,
    inputs: &[LEVEL.definition()],
};
static COUPLED: Definition<Controls> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Input(0),
        &Expr::Binary(Binary::Greater, &Expr::Input(1), &Expr::Constant(ZERO)),
    ),
    arguments: &[],
    inputs: <(bool, u8) as Arguments>::TYPES,
};
static POWER_LEVEL: Predicate<Controls> = Predicate {
    definition: &COUPLED,
    inputs: &[POWER.definition(), LEVEL.definition()],
};
static PERMITTED: Definition<Controls> = Definition {
    body: Expr::Binary(
        Binary::LessEqual,
        &Expr::Argument(0),
        &Expr::Constant(LIMIT),
    ),
    arguments: <(u8,) as Arguments>::TYPES,
    inputs: &[],
};
struct SetPower;
impl StateTransformer for SetPower {
    type Appliance = Controls;
    type Arguments = (bool,);
    type Error = ();
    const DEFINITION: Operation<Controls> = Operation {
        precondition: Predicate {
            definition: &TRUE,
            inputs: &[],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[POWER.definition()],
            results: &[Expr::Argument(0)],
            arguments: Self::Arguments::TYPES,
        },
    };
    fn invoke(a: &Controls, (power,): (bool,)) -> Result<(), ()> {
        a.power.store(power, Relaxed);
        Ok(())
    }
}
struct SetLevel;
impl StateTransformer for SetLevel {
    type Appliance = Controls;
    type Arguments = (u8,);
    type Error = ();
    const DEFINITION: Operation<Controls> = Operation {
        precondition: Predicate {
            definition: &PERMITTED,
            inputs: &[],
        },
        effect: Effect {
            inputs: &[],
            outputs: &[POWER.definition(), LEVEL.definition()],
            results: &[
                Expr::Binary(Binary::Greater, &Expr::Argument(0), &Expr::Constant(ZERO)),
                Expr::Argument(0),
            ],
            arguments: Self::Arguments::TYPES,
        },
    };
    fn invoke(a: &Controls, (level,): (u8,)) -> Result<(), ()> {
        if level > 200 {
            return Err(());
        }
        a.level.store(level, Relaxed);
        a.power.store(level != 0, Relaxed);
        Ok(())
    }
}
static SET_POWER: TransformerHandle<Controls, ()> = TransformerHandle::new::<SetPower>();
static SET_LEVEL: TransformerHandle<Controls, ()> = TransformerHandle::new::<SetLevel>();
pub static SWITCH: Surface<'static, Controls, ()> = Surface {
    states: &[&POWER],
    transformers: &[&SET_POWER],
    axioms: &[],
    invariants: &[],
};
pub static DIMMER: Surface<'static, Controls, ()> = Surface {
    states: &[&POWER, &LEVEL],
    transformers: &[&SET_LEVEL],
    axioms: &[&POWER_LEVEL],
    invariants: &[&SAFE_LEVEL],
};

/// Initialize the selected native fixture before ordinary kernel startup.
pub fn appliance(
    context: &lattice_vertex::sync::TaskContext,
    dimmer: bool,
) -> &'static lattice_vertex::kernel::Appliance<Controls, ()> {
    static SWITCH_NATIVE: lattice_vertex::kernel::Appliance<Controls, ()> =
        lattice_vertex::kernel::Appliance::new(&SWITCH);
    static DIMMER_NATIVE: lattice_vertex::kernel::Appliance<Controls, ()> =
        lattice_vertex::kernel::Appliance::new(&DIMMER);
    let appliance = if dimmer {
        &DIMMER_NATIVE
    } else {
        &SWITCH_NATIVE
    };
    appliance.initialize(
        context,
        Controls {
            power: AtomicBool::new(false),
            level: AtomicU8::new(0),
        },
    );
    if dimmer {
        appliance
            .install_state(
                context,
                &LEVEL_HISTORY,
                level_difference,
                &LEVEL_DIFFERENCE_RULE,
            )
            .unwrap();
    }
    appliance
}

static SENT: AtomicU32 = AtomicU32::new(0);
static RECEIVED: AtomicU32 = AtomicU32::new(0);
static READS: AtomicU32 = AtomicU32::new(0);
static FORWARDED: AtomicU32 = AtomicU32::new(0);
static ARRIVALS: AtomicU32 = AtomicU32::new(0);
static TRACE_DROPPED: AtomicU32 = AtomicU32::new(0);

/// One encoded range observed on the radio task, including forwarded replies.
/// Reception means wire validation, not complete delivery or a retained surface.
#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub tick: lattice_vertex::types::TickType_t,
    pub sent: bool,
    pub reader: u64,
    pub owner: u64,
    pub request: u16,
    pub offset: u16,
    pub total: u16,
    pub length: u16,
    pub rssi_dbm: Option<i8>,
    pub hash: u32,
}
static mut PARTS: [Option<Part>; 32] = [None; 32];
static mut TRACE_READ: usize = 0;
static mut TRACE_WRITE: usize = 0;
static mut TRACE_COUNT: usize = 0;

fn trace(part: Part) {
    // Called only by the radio task. The reporter masks preemption while
    // removing a single record; formatting happens after that critical section.
    unsafe {
        if TRACE_COUNT == 32 {
            TRACE_READ = (TRACE_READ + 1) % 32;
            TRACE_COUNT -= 1;
            TRACE_DROPPED.fetch_add(1, Relaxed);
        }
        PARTS[TRACE_WRITE] = Some(part);
        TRACE_WRITE = (TRACE_WRITE + 1) % 32;
        TRACE_COUNT += 1;
    }
}

pub fn take_part(context: &lattice_vertex::sync::TaskContext) -> Option<Part> {
    let _ = context;
    lattice_vertex::port::portENTER_CRITICAL();
    let part = unsafe {
        if TRACE_COUNT == 0 {
            None
        } else {
            let part = PARTS[TRACE_READ].take();
            TRACE_READ = (TRACE_READ + 1) % 32;
            TRACE_COUNT -= 1;
            part
        }
    };
    lattice_vertex::port::portEXIT_CRITICAL();
    part
}

pub(super) fn event(event: Event<'_>) {
    let (sent, reader, owner, request, offset, total, bytes) = match event {
        Event::ArrivalReceived => {
            ARRIVALS.fetch_add(1, Relaxed);
            return;
        }
        Event::SurfaceReadReceived { .. } => {
            READS.fetch_add(1, Relaxed);
            return;
        }
        Event::SurfaceReadForwarded => {
            FORWARDED.fetch_add(1, Relaxed);
            return;
        }
        Event::SurfacePartSent {
            reader,
            owner,
            request,
            offset,
            total,
            bytes,
        } => {
            SENT.fetch_add(1, Relaxed);
            (true, reader, owner, request, offset, total, bytes)
        }
        Event::SurfacePartReceived {
            reader,
            owner,
            request,
            offset,
            total,
            bytes,
        } => {
            RECEIVED.fetch_add(1, Relaxed);
            (false, reader, owner, request, offset, total, bytes)
        }
        _ => return,
    };
    trace(Part {
        tick: lattice_vertex::kernel::tasks::xTaskGetTickCount(),
        sent,
        reader,
        owner,
        request,
        offset,
        total,
        length: bytes.len() as u16,
        rssi_dbm: if sent {
            None
        } else {
            super::discovery::last_signal()
        },
        hash: bytes.iter().fold(0x811c9dc5u32, |h, b| {
            (h ^ u32::from(*b)).wrapping_mul(0x01000193)
        }),
    });
}

#[derive(Debug)]
pub struct Statistics {
    pub reads_received: u32,
    pub reads_forwarded: u32,
    pub parts_sent: u32,
    pub parts_received: u32,
    pub trace_dropped: u32,
    pub arrivals: u32,
}
pub fn statistics(context: &lattice_vertex::sync::TaskContext) -> Statistics {
    let _ = context;
    Statistics {
        reads_received: READS.load(Relaxed),
        reads_forwarded: FORWARDED.load(Relaxed),
        parts_sent: SENT.load(Relaxed),
        parts_received: RECEIVED.load(Relaxed),
        trace_dropped: TRACE_DROPPED.load(Relaxed),
        arrivals: ARRIVALS.load(Relaxed),
    }
}
