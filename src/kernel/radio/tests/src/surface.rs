//! Two authored test appliances and transient surface-transfer checks.
//! Only the kernel emits packets. The observer retains a checksum and the
//! 13-byte description header, never a remote mathematical surface.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};
use lattice_vertex::{
    ir::{Binary, Effect, Expr, Integer, Value},
    kernel::radio::testing::Event,
    surface::{
        Arguments, Definition, Operation, Predicate, State, StateHandle, StateTransformer, Surface,
        TransformerHandle,
    },
};

pub struct Appliance {
    power: bool,
    level: u8,
}
struct Power;
impl State for Power {
    type Appliance = Appliance;
    type Value = bool;
    fn read(a: &Appliance) -> Option<bool> {
        Some(a.power)
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
static POWER: StateHandle<Appliance> = StateHandle::new::<Power>();
static LEVEL: StateHandle<Appliance> = StateHandle::new::<Level>();
const ZERO: Value = match Value::integer(Integer::U8, 0) {
    Ok(v) => v,
    Err(_) => panic!(),
};
const LIMIT: Value = match Value::integer(Integer::U8, 200) {
    Ok(v) => v,
    Err(_) => panic!(),
};
static TRUE: Definition<Appliance> = Definition {
    body: Expr::Constant(Value::boolean(true)),
    arguments: &[],
    inputs: &[],
};
static SAFE: Definition<Appliance> = Definition {
    body: Expr::Binary(Binary::LessEqual, &Expr::Input(0), &Expr::Constant(LIMIT)),
    arguments: &[],
    inputs: <(u8,) as Arguments>::TYPES,
};
static SAFE_LEVEL: Predicate<Appliance> = Predicate {
    definition: &SAFE,
    inputs: &[LEVEL.definition()],
};
static COUPLED: Definition<Appliance> = Definition {
    body: Expr::Binary(
        Binary::Equal,
        &Expr::Input(0),
        &Expr::Binary(Binary::Greater, &Expr::Input(1), &Expr::Constant(ZERO)),
    ),
    arguments: &[],
    inputs: <(bool, u8) as Arguments>::TYPES,
};
static POWER_LEVEL: Predicate<Appliance> = Predicate {
    definition: &COUPLED,
    inputs: &[POWER.definition(), LEVEL.definition()],
};
static PERMITTED: Definition<Appliance> = Definition {
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
    type Appliance = Appliance;
    type Arguments = (bool,);
    type Error = ();
    const DEFINITION: Operation<Appliance> = Operation {
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
    fn invoke(a: &mut Appliance, (power,): (bool,)) -> Result<(), ()> {
        a.power = power;
        Ok(())
    }
}
struct SetLevel;
impl StateTransformer for SetLevel {
    type Appliance = Appliance;
    type Arguments = (u8,);
    type Error = ();
    const DEFINITION: Operation<Appliance> = Operation {
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
    fn invoke(a: &mut Appliance, (level,): (u8,)) -> Result<(), ()> {
        if level > 200 {
            return Err(());
        }
        a.level = level;
        a.power = level != 0;
        Ok(())
    }
}
static SET_POWER: TransformerHandle<Appliance, ()> = TransformerHandle::new::<SetPower>();
static SET_LEVEL: TransformerHandle<Appliance, ()> = TransformerHandle::new::<SetLevel>();
pub static SWITCH: Surface<'static, Appliance, ()> = Surface {
    states: &[&POWER],
    transformers: &[&SET_POWER],
    axioms: &[],
    invariants: &[],
};
pub static DIMMER: Surface<'static, Appliance, ()> = Surface {
    states: &[&POWER, &LEVEL],
    transformers: &[&SET_LEVEL],
    axioms: &[&POWER_LEVEL],
    invariants: &[&SAFE_LEVEL],
};

static SENT: AtomicU32 = AtomicU32::new(0);
static RECEIVED: AtomicU32 = AtomicU32::new(0);
static INCOMPLETE: AtomicU32 = AtomicU32::new(0);
static ARRIVALS: AtomicU32 = AtomicU32::new(0);
static TX_HASH: AtomicU32 = AtomicU32::new(0);
static RX_HASH: AtomicU32 = AtomicU32::new(0);
static RX_COUNTS: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];

struct Transfer {
    source: u64,
    sequence: u16,
    total: u16,
    next: u16,
    hash: u32,
    header: [u8; 13],
    active: bool,
}
impl Transfer {
    const fn new() -> Self {
        Self {
            source: 0,
            sequence: 0,
            total: 0,
            next: 0,
            hash: 0,
            header: [0; 13],
            active: false,
        }
    }
    fn piece(
        &mut self,
        source: u64,
        sequence: u16,
        offset: u16,
        total: u16,
        bytes: &[u8],
    ) -> Result<bool, ()> {
        if offset == 0 {
            *self = Self {
                source,
                sequence,
                total,
                next: 0,
                hash: 0x811c9dc5,
                header: [0; 13],
                active: true,
            };
        }
        if !self.active
            || (source, sequence, total, offset)
                != (self.source, self.sequence, self.total, self.next)
        {
            self.active = false;
            return Err(());
        }
        for &byte in bytes {
            if (self.next as usize) < self.header.len() {
                self.header[self.next as usize] = byte;
            }
            self.hash = (self.hash ^ u32::from(byte)).wrapping_mul(0x01000193);
            self.next += 1;
        }
        if self.next == self.total {
            self.active = false;
            if &self.header[..4] != b"LSF\x01" || !matches!(self.header[4], 32 | 64) {
                return Err(());
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
// These events are emitted only by the resident task. IRQ callbacks and the
// reporter never access transfer progress; completed measurements are atomic.
static mut TX: Transfer = Transfer::new();
static mut RX: Transfer = Transfer::new();

pub(super) fn event(event: Event<'_>) {
    match event {
        Event::ArrivalReceived => {
            ARRIVALS.fetch_add(1, Relaxed);
        }
        Event::SurfacePartSent {
            sequence,
            offset,
            total,
            bytes,
        } => {
            let tx = unsafe { &mut *core::ptr::addr_of_mut!(TX) };
            if tx.piece(0, sequence, offset, total, bytes) == Ok(true) {
                TX_HASH.store(tx.hash, Relaxed);
                SENT.fetch_add(1, Relaxed);
            }
        }
        Event::SurfacePartReceived {
            source,
            sequence,
            offset,
            total,
            bytes,
        } => {
            let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX) };
            match rx.piece(source, sequence, offset, total, bytes) {
                Ok(true) => {
                    RX_HASH.store(rx.hash, Relaxed);
                    for (i, count) in RX_COUNTS.iter().enumerate() {
                        count.store(
                            u16::from_le_bytes(rx.header[5 + 2 * i..7 + 2 * i].try_into().unwrap())
                                as u32,
                            Relaxed,
                        );
                    }
                    RECEIVED.fetch_add(1, Relaxed);
                }
                Err(()) => {
                    INCOMPLETE.fetch_add(1, Relaxed);
                }
                Ok(false) => {}
            }
        }
        _ => {}
    }
}

#[derive(Debug)]
pub struct Statistics {
    pub sent: u32,
    pub received: u32,
    pub incomplete: u32,
    pub arrivals: u32,
    pub tx_hash: u32,
    pub rx_hash: u32,
    pub states: u32,
    pub transformers: u32,
    pub axioms: u32,
    pub invariants: u32,
}
pub fn statistics(context: &lattice_vertex::sync::TaskContext) -> Statistics {
    let _ = context;
    lattice_vertex::port::portENTER_CRITICAL();
    let result = Statistics {
        sent: SENT.load(Relaxed),
        received: RECEIVED.load(Relaxed),
        incomplete: INCOMPLETE.load(Relaxed),
        arrivals: ARRIVALS.load(Relaxed),
        tx_hash: TX_HASH.load(Relaxed),
        rx_hash: RX_HASH.load(Relaxed),
        states: RX_COUNTS[0].load(Relaxed),
        transformers: RX_COUNTS[1].load(Relaxed),
        axioms: RX_COUNTS[2].load(Relaxed),
        invariants: RX_COUNTS[3].load(Relaxed),
    };
    lattice_vertex::port::portEXIT_CRITICAL();
    result
}
