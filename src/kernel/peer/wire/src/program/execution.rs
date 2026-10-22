//! Execution control and receiver-reserved handoffs over the common program route.
//! A receiver ticket can be committed once. Reusing an expired ticket never starts
//! another visit; a sender must report that uncertain outcome rather than re-reserve.

use super::transfer::REQUEST_BYTES;

pub const CHUNK_BYTES: usize = 38;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub definition: u16,
    pub generation: u32,
    pub record: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub epoch: u64,
    pub slot: u16,
    pub generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cause {
    pub owner: u64,
    pub epoch: u64,
    pub sequence: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    State {
        definition: u16,
        generation: u32,
    },
    Enable {
        definition: u16,
        generation: u32,
        revision: u32,
        enabled: bool,
    },
    Session,
    Reserve {
        epoch: u64,
        sender_epoch: u64,
        sequence: u32,
        cause: Cause,
        entry: Entry,
        upstream: bool,
        length: u16,
    },
    Data {
        ticket: Ticket,
        offset: u16,
        bytes: &'a [u8],
    },
    Commit {
        ticket: Ticket,
        digest: [u8; 32],
    },
    Release {
        ticket: Ticket,
    },
    Statistics,
}
impl Request<'_> {
    pub fn encode(self, out: &mut [u8; REQUEST_BYTES]) -> usize {
        match self {
            Self::State {
                definition,
                generation,
            } => {
                out[0] = 7;
                out[1..3].copy_from_slice(&definition.to_le_bytes());
                out[3..7].copy_from_slice(&generation.to_le_bytes());
                7
            }
            Self::Enable {
                definition,
                generation,
                revision,
                enabled,
            } => {
                out[0] = 8;
                out[1..3].copy_from_slice(&definition.to_le_bytes());
                out[3..7].copy_from_slice(&generation.to_le_bytes());
                out[7..11].copy_from_slice(&revision.to_le_bytes());
                out[11] = enabled as u8;
                12
            }
            Self::Session => {
                out[0] = 9;
                1
            }
            Self::Reserve {
                epoch,
                sender_epoch,
                sequence,
                cause,
                entry,
                upstream,
                length,
            } => {
                out[0] = 10;
                out[1..9].copy_from_slice(&epoch.to_le_bytes());
                out[9..17].copy_from_slice(&sender_epoch.to_le_bytes());
                out[17..21].copy_from_slice(&sequence.to_le_bytes());
                out[21..23].copy_from_slice(&entry.definition.to_le_bytes());
                out[23..27].copy_from_slice(&entry.generation.to_le_bytes());
                out[27..29].copy_from_slice(&entry.record.to_le_bytes());
                out[29] = upstream as u8;
                out[30..32].copy_from_slice(&length.to_le_bytes());
                out[32..40].copy_from_slice(&cause.owner.to_le_bytes());
                out[40..48].copy_from_slice(&cause.epoch.to_le_bytes());
                out[48..52].copy_from_slice(&cause.sequence.to_le_bytes());
                52
            }
            Self::Data {
                ticket,
                offset,
                bytes,
            } => {
                assert!(!bytes.is_empty() && bytes.len() <= CHUNK_BYTES);
                out[0] = 11;
                write_ticket(ticket, out);
                out[15..17].copy_from_slice(&offset.to_le_bytes());
                out[17] = bytes.len() as u8;
                out[18..18 + bytes.len()].copy_from_slice(bytes);
                18 + bytes.len()
            }
            Self::Commit { ticket, digest } => {
                out[0] = 12;
                write_ticket(ticket, out);
                out[15..47].copy_from_slice(&digest);
                47
            }
            Self::Release { ticket } => {
                out[0] = 13;
                write_ticket(ticket, out);
                15
            }
            Self::Statistics => {
                out[0] = 14;
                1
            }
        }
    }
}
fn write_ticket(t: Ticket, b: &mut [u8]) {
    b[1..9].copy_from_slice(&t.epoch.to_le_bytes());
    b[9..11].copy_from_slice(&t.slot.to_le_bytes());
    b[11..15].copy_from_slice(&t.generation.to_le_bytes());
}
fn ticket(b: &[u8]) -> Option<Ticket> {
    Some(Ticket {
        epoch: u64::from_le_bytes(b.get(1..9)?.try_into().ok()?),
        slot: u16::from_le_bytes(b.get(9..11)?.try_into().ok()?),
        generation: u32::from_le_bytes(b.get(11..15)?.try_into().ok()?),
    })
}
impl<'a> Request<'a> {
    pub fn decode(b: &'a [u8]) -> Option<Self> {
        Some(match (*b.first()?, b.len()) {
            (7, 7) => Self::State {
                definition: u16::from_le_bytes(b[1..3].try_into().ok()?),
                generation: u32::from_le_bytes(b[3..7].try_into().ok()?),
            },
            (8, 12) if b[11] <= 1 => Self::Enable {
                definition: u16::from_le_bytes(b[1..3].try_into().ok()?),
                generation: u32::from_le_bytes(b[3..7].try_into().ok()?),
                revision: u32::from_le_bytes(b[7..11].try_into().ok()?),
                enabled: b[11] != 0,
            },
            (9, 1) => Self::Session,
            (10, 52) if b[29] <= 1 => Self::Reserve {
                epoch: u64::from_le_bytes(b[1..9].try_into().ok()?),
                sender_epoch: u64::from_le_bytes(b[9..17].try_into().ok()?),
                sequence: u32::from_le_bytes(b[17..21].try_into().ok()?),
                cause: Cause {
                    owner: u64::from_le_bytes(b[32..40].try_into().ok()?),
                    epoch: u64::from_le_bytes(b[40..48].try_into().ok()?),
                    sequence: u32::from_le_bytes(b[48..52].try_into().ok()?),
                },
                entry: Entry {
                    definition: u16::from_le_bytes(b[21..23].try_into().ok()?),
                    generation: u32::from_le_bytes(b[23..27].try_into().ok()?),
                    record: u16::from_le_bytes(b[27..29].try_into().ok()?),
                },
                upstream: b[29] != 0,
                length: u16::from_le_bytes(b[30..32].try_into().ok()?),
            },
            (11, n) if (19..=REQUEST_BYTES).contains(&n) && b[17] as usize == n - 18 => {
                Self::Data {
                    ticket: ticket(b)?,
                    offset: u16::from_le_bytes(b[15..17].try_into().ok()?),
                    bytes: &b[18..],
                }
            }
            (12, 47) => Self::Commit {
                ticket: ticket(b)?,
                digest: b[15..47].try_into().ok()?,
            },
            (13, 15) => Self::Release { ticket: ticket(b)? },
            (14, 1) => Self::Statistics,
            _ => return None,
        })
    }
}
