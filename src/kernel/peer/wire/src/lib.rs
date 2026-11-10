//! Numeric transport definitions; no radio driver, RTOS, interpreter, or allocator.
#![no_std]
pub use lattice_ir::{Integer, Type, Value};
pub mod clock;
pub mod control;
pub mod cost;
#[cfg(feature = "host")]
pub mod host;
pub mod program;
pub mod scalar;
pub mod update;
pub const CALL_ARGUMENT_BYTES: usize = 61;
pub const MAX_CALL_ARGUMENTS: usize = 30;
pub const SURFACE_BYTES: usize = 63;

/// Implementor-declared IR availability. DeclaredComplete is a claim for a
/// synthesis consumer to validate, never kernel certification of model correctness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum IrStatus {
    Absent = 0,
    Incomplete = 1,
    DeclaredComplete = 2,
}
impl IrStatus {
    pub fn decode(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Absent,
            1 => Self::Incomplete,
            2 => Self::DeclaredComplete,
            _ => return None,
        })
    }
}

/// Separate optional presentation metadata from an LSF definition without
/// interpreting IR. The returned definition retains its original version byte.
/// For a v4 compatibility hash, replace that byte with 3 and hash only this prefix.
/// Full record validation remains the surface consumer's responsibility.
pub fn surface_parts(bytes: &[u8]) -> Option<(&[u8], Option<&str>)> {
    if bytes.len() > u16::MAX as usize || bytes.get(4) != Some(&64) {
        return None;
    }
    match bytes.get(..4)? {
        b"LSF\x02" if bytes.len() >= 13 => Some((bytes, None)),
        b"LSF\x03" if bytes.len() >= 14 => {
            IrStatus::decode(bytes[5])?;
            Some((bytes, None))
        }
        b"LSF\x04" if bytes.len() >= 16 => {
            IrStatus::decode(bytes[5])?;
            let end = bytes.len() - 2;
            let length = u16::from_le_bytes(bytes[end..].try_into().ok()?) as usize;
            let start = end.checked_sub(length).filter(|&n| n >= 14)?;
            let name = core::str::from_utf8(&bytes[start..end]).ok()?;
            if name.as_bytes().contains(&0) {
                return None;
            }
            Some((&bytes[..start], (!name.is_empty()).then_some(name)))
        }
        _ => None,
    }
}

/// Result of dispatch, never a claim that physical state has converged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum InvokeStatus {
    Invoked = 0,
    Arguments = 1,
    UnknownTransformer = 2,
    ApplianceError = 3,
    Busy = 4,
    ConflictingRequest = 5,
}

/// Established state and the owner's change watermark, or a distinct read failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateReadResult {
    Value { sequence: u32, value: Option<Value> },
    UnknownAttribute,
    Busy,
}

/// One registered change, identified by its owner and owner-local sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateChange {
    pub owner: u64,
    pub attribute: u64,
    pub sequence: u32,
    pub value: Option<Value>,
}
