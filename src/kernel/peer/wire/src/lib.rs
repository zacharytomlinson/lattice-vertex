//! Numeric transport definitions; no radio driver, RTOS, interpreter, or allocator.
#![no_std]
pub use lattice_ir::{Integer, Type, Value};
pub mod cost;
#[cfg(feature = "host")]
pub mod host;
pub mod scalar;
pub const CALL_ARGUMENT_BYTES: usize = 61;
pub const MAX_CALL_ARGUMENTS: usize = 30;
pub const SURFACE_BYTES: usize = 63;

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
