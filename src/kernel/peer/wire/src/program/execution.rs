//! Program admission and current connection updates. Local operation records
//! remain unchanged; retired visit-transfer commands (10..=13) are not accepted.
use super::transfer::REQUEST_BYTES;

pub const SIGNAL_BYTES: usize = REQUEST_BYTES - 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub definition: u16,
    pub generation: u32,
    pub record: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    State { definition: u16, generation: u32 },
    Enable { definition: u16, generation: u32, revision: u32, enabled: bool },
    Session,
    Signal {
        source: Entry,
        target: Entry,
        epoch: u64,
        sequence: u32,
        active: bool,
        offset: u16,
        bytes: &'a [u8],
    },
    Statistics,
}
fn write_entry(entry: Entry, b: &mut [u8]) {
    b[..2].copy_from_slice(&entry.definition.to_le_bytes());
    b[2..6].copy_from_slice(&entry.generation.to_le_bytes());
    b[6..8].copy_from_slice(&entry.record.to_le_bytes());
}
fn entry(b: &[u8]) -> Option<Entry> {
    Some(Entry {
        definition: u16::from_le_bytes(b.get(..2)?.try_into().ok()?),
        generation: u32::from_le_bytes(b.get(2..6)?.try_into().ok()?),
        record: u16::from_le_bytes(b.get(6..8)?.try_into().ok()?),
    })
}
impl Request<'_> {
    pub fn encode(self, out: &mut [u8; REQUEST_BYTES]) -> usize {
        match self {
            Self::State { definition, generation } => {
                out[0] = 7;
                out[1..3].copy_from_slice(&definition.to_le_bytes());
                out[3..7].copy_from_slice(&generation.to_le_bytes());
                7
            }
            Self::Enable { definition, generation, revision, enabled } => {
                out[0] = 8;
                out[1..3].copy_from_slice(&definition.to_le_bytes());
                out[3..7].copy_from_slice(&generation.to_le_bytes());
                out[7..11].copy_from_slice(&revision.to_le_bytes());
                out[11] = enabled as u8;
                12
            }
            Self::Session => { out[0] = 9; 1 }
            Self::Signal { source, target, epoch, sequence, active, offset, bytes } => {
                assert!(bytes.len() <= SIGNAL_BYTES);
                assert!(active || (offset == 0 && bytes.is_empty()));
                out[0] = 30;
                write_entry(source, &mut out[1..9]);
                write_entry(target, &mut out[9..17]);
                out[17..25].copy_from_slice(&epoch.to_le_bytes());
                out[25..29].copy_from_slice(&sequence.to_le_bytes());
                out[29] = active as u8;
                out[30..32].copy_from_slice(&offset.to_le_bytes());
                out[32..32 + bytes.len()].copy_from_slice(bytes);
                32 + bytes.len()
            }
            Self::Statistics => { out[0] = 14; 1 }
        }
    }
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
            (14, 1) => Self::Statistics,
            (30, 32..=REQUEST_BYTES) if b[29] <= 1 => {
                let offset = u16::from_le_bytes(b[30..32].try_into().ok()?);
                let active = b[29] != 0;
                if !active && (offset != 0 || b.len() != 32) { return None; }
                Self::Signal {
                    source: entry(&b[1..9])?,
                    target: entry(&b[9..17])?,
                    epoch: u64::from_le_bytes(b[17..25].try_into().ok()?),
                    sequence: u32::from_le_bytes(b[25..29].try_into().ok()?),
                    active, offset, bytes: &b[32..],
                }
            }
            _ => return None,
        })
    }
}
