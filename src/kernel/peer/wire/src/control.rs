//! Continuous absolute-target control. Pending targets may be superseded;
//! running native calls and registered physical deltas are never canceled.
pub use crate::program::execution::Ticket;
use crate::program::transfer::{Reply, Status, REQUEST_BYTES};
use crate::{scalar, InvokeStatus, StateReadResult, Type, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Newest target retained by the receiver, not a promise to execute intermediates.
    pub received: u32,
    /// Most recent completed native call. May jump over superseded targets.
    pub completed: u32,
    pub result: InvokeStatus,
    pub finished: bool,
    pub state: StateReadResult,
}
impl Progress {
    pub fn encode(self, out: &mut [u8]) -> usize {
        out[..4].copy_from_slice(&self.received.to_le_bytes());
        out[4..8].copy_from_slice(&self.completed.to_le_bytes());
        out[8] = self.result as u8;
        out[9] = self.finished as u8;
        match self.state {
            StateReadResult::Value { sequence, value } => {
                out[10] = 0;
                out[11..15].copy_from_slice(&sequence.to_le_bytes());
                if let Some(v) = value {
                    15 + scalar::encode_value(v, &mut out[15..])
                } else {
                    out[15] = 255;
                    16
                }
            }
            StateReadResult::UnknownAttribute => {
                out[10] = 1;
                11
            }
            StateReadResult::Busy => {
                out[10] = 2;
                11
            }
        }
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < 11 || b[9] > 1 {
            return None;
        }
        let result = match b[8] {
            0 => InvokeStatus::Invoked,
            1 => InvokeStatus::Arguments,
            2 => InvokeStatus::UnknownTransformer,
            3 => InvokeStatus::ApplianceError,
            4 => InvokeStatus::Busy,
            5 => InvokeStatus::ConflictingRequest,
            _ => return None,
        };
        let state = match b[10] {
            0 if b.len() >= 16 => {
                let value = if b[15] == 255 && b.len() == 16 {
                    None
                } else {
                    let (v, n) = scalar::decode_value(&b[15..])?;
                    if n + 15 != b.len() {
                        return None;
                    }
                    Some(v)
                };
                StateReadResult::Value {
                    sequence: u32::from_le_bytes(b[11..15].try_into().ok()?),
                    value,
                }
            }
            1 if b.len() == 11 => StateReadResult::UnknownAttribute,
            2 if b.len() == 11 => StateReadResult::Busy,
            _ => return None,
        };
        let received = u32::from_le_bytes(b[..4].try_into().ok()?);
        let completed = u32::from_le_bytes(b[4..8].try_into().ok()?);
        if completed > received || (b[9] != 0 && completed != received) {
            return None;
        }
        Some(Self {
            received,
            completed,
            result,
            finished: b[9] != 0,
            state,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feedback {
    pub owner: u64,
    pub nonce: u64,
    pub ticket: Ticket,
    pub revision: u32,
    pub progress: Progress,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Open {
        epoch: u64,
        nonce: u64,
        transformer: u64,
        state: u64,
    },
    Target {
        ticket: Ticket,
        sequence: u32,
        value: Value,
    },
    Finish {
        ticket: Ticket,
        sequence: u32,
    },
    Close {
        ticket: Ticket,
        nonce: u64,
    },
    Feedback {
        nonce: u64,
        ticket: Ticket,
        revision: u32,
        progress: Progress,
    },
}
fn ticket(out: &mut [u8], t: Ticket) {
    out[..8].copy_from_slice(&t.epoch.to_le_bytes());
    out[8..10].copy_from_slice(&t.slot.to_le_bytes());
    out[10..14].copy_from_slice(&t.generation.to_le_bytes());
}
fn read_ticket(b: &[u8]) -> Option<Ticket> {
    Some(Ticket {
        epoch: u64::from_le_bytes(b.get(..8)?.try_into().ok()?),
        slot: u16::from_le_bytes(b.get(8..10)?.try_into().ok()?),
        generation: u32::from_le_bytes(b.get(10..14)?.try_into().ok()?),
    })
}
impl Request {
    pub fn encode(self, out: &mut [u8; REQUEST_BYTES]) -> usize {
        match self {
            Self::Open {
                epoch,
                nonce,
                transformer,
                state,
            } => {
                out[0] = 19;
                for (n, v) in [epoch, nonce, transformer, state].into_iter().enumerate() {
                    out[1 + n * 8..9 + n * 8].copy_from_slice(&v.to_le_bytes());
                }
                33
            }
            Self::Target {
                ticket: t,
                sequence,
                value,
            } => {
                out[0] = 20;
                ticket(&mut out[1..], t);
                out[15..19].copy_from_slice(&sequence.to_le_bytes());
                19 + scalar::encode_value(value, &mut out[19..])
            }
            Self::Finish {
                ticket: t,
                sequence,
            } => {
                out[0] = 21;
                ticket(&mut out[1..], t);
                out[15..19].copy_from_slice(&sequence.to_le_bytes());
                19
            }
            Self::Close { ticket: t, nonce } => {
                out[0] = 22;
                ticket(&mut out[1..], t);
                out[15..23].copy_from_slice(&nonce.to_le_bytes());
                23
            }
            Self::Feedback {
                nonce,
                ticket: t,
                revision,
                progress,
            } => {
                out[0] = 23;
                out[1..9].copy_from_slice(&nonce.to_le_bytes());
                ticket(&mut out[9..], t);
                out[23..27].copy_from_slice(&revision.to_le_bytes());
                27 + progress.encode(&mut out[27..])
            }
        }
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        Some(match (*b.first()?, b.len()) {
            (19, 33) => Self::Open {
                epoch: u64::from_le_bytes(b[1..9].try_into().ok()?),
                nonce: u64::from_le_bytes(b[9..17].try_into().ok()?),
                transformer: u64::from_le_bytes(b[17..25].try_into().ok()?),
                state: u64::from_le_bytes(b[25..33].try_into().ok()?),
            },
            (20, 21..=28) => {
                let (value, n) = scalar::decode_value(&b[19..])?;
                if n + 19 != b.len() {
                    return None;
                }
                Self::Target {
                    ticket: read_ticket(&b[1..])?,
                    sequence: u32::from_le_bytes(b[15..19].try_into().ok()?),
                    value,
                }
            }
            (21, 19) => Self::Finish {
                ticket: read_ticket(&b[1..])?,
                sequence: u32::from_le_bytes(b[15..19].try_into().ok()?),
            },
            (22, 23) => Self::Close {
                ticket: read_ticket(&b[1..])?,
                nonce: u64::from_le_bytes(b[15..23].try_into().ok()?),
            },
            (23, 38..=51) => Self::Feedback {
                nonce: u64::from_le_bytes(b[1..9].try_into().ok()?),
                ticket: read_ticket(&b[9..])?,
                revision: u32::from_le_bytes(b[23..27].try_into().ok()?),
                progress: Progress::decode(&b[27..])?,
            },
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub ticket: Ticket,
    pub input: Type,
    pub revision: u32,
    pub progress: Progress,
}
impl State {
    pub fn reply<'a>(self, status: Status, out: &'a mut [u8; 48]) -> Reply<'a> {
        out[..8].copy_from_slice(&self.ticket.epoch.to_le_bytes());
        out[8] = scalar::type_tag(self.input);
        out[9..13].copy_from_slice(&self.revision.to_le_bytes());
        let n = 13 + self.progress.encode(&mut out[13..]);
        Reply {
            offset: self.ticket.slot,
            generation: self.ticket.generation,
            bytes: &out[..n],
            ..Reply::status(status)
        }
    }
    pub fn decode(r: Reply<'_>) -> Option<Self> {
        if r.bytes.len() < 24 {
            return None;
        }
        let input = scalar::decode_type(r.bytes[8])?;
        Some(Self {
            ticket: Ticket {
                epoch: u64::from_le_bytes(r.bytes[..8].try_into().ok()?),
                slot: r.offset,
                generation: r.generation,
            },
            input,
            revision: u32::from_le_bytes(r.bytes[9..13].try_into().ok()?),
            progress: Progress::decode(&r.bytes[13..])?,
        })
    }
}

/// An asynchronous host input: receipt on USB retains current intent, not an invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub owner: u64,
    pub nonce: u64,
    pub ticket: Ticket,
    pub sequence: u32,
    pub value: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_bounds() {
        let ticket = Ticket {
            epoch: 8,
            slot: 2,
            generation: 3,
        };
        for value in [
            Value::boolean(false),
            Value::integer(crate::Integer::U64, u64::MAX as i128).unwrap(),
        ] {
            let progress = Progress {
                received: 3,
                completed: 3,
                result: InvokeStatus::Invoked,
                finished: true,
                state: StateReadResult::Value {
                    sequence: 4,
                    value: Some(value),
                },
            };
            for r in [
                Request::Open {
                    epoch: 8,
                    nonce: 5,
                    transformer: 7,
                    state: 9,
                },
                Request::Target {
                    ticket,
                    sequence: 3,
                    value,
                },
                Request::Finish {
                    ticket,
                    sequence: 3,
                },
                Request::Close { ticket, nonce: 5 },
                Request::Feedback {
                    nonce: 5,
                    ticket,
                    revision: 8,
                    progress,
                },
            ] {
                let mut out = [0; REQUEST_BYTES];
                let n = r.encode(&mut out);
                assert!(n <= REQUEST_BYTES);
                assert_eq!(Request::decode(&out[..n]), Some(r));
                for end in 0..n {
                    assert!(Request::decode(&out[..end]).is_none());
                }
                assert!(Request::decode(&out[..n + 1]).is_none());
            }
            let state = State {
                ticket,
                input: value.ty(),
                revision: 8,
                progress,
            };
            let mut bytes = [0; 48];
            assert_eq!(
                State::decode(state.reply(Status::Ok, &mut bytes)),
                Some(state)
            );
            #[cfg(feature = "host")]
            for p in [
                crate::host::Packet::ControlTarget {
                    target: Target {
                        owner: 2,
                        nonce: 5,
                        ticket,
                        sequence: 3,
                        value,
                    },
                },
                crate::host::Packet::ControlProgress {
                    feedback: Feedback {
                        owner: 2,
                        nonce: 5,
                        ticket,
                        revision: 8,
                        progress,
                    },
                },
                crate::host::Packet::ControlFault {
                    owner: 2,
                    nonce: 5,
                    status: Status::Stale,
                },
            ] {
                let mut b = [0; crate::host::MAX_PACKET];
                let n = p.encode(&mut b);
                let mut d = crate::host::Decoder::new();
                let mut got = b[..n].iter().filter_map(|v| d.push(*v));
                assert_eq!(got.next(), Some(p));
                assert_eq!(got.next(), None);
            }
        }
    }
}
