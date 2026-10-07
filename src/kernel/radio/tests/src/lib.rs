//! Shared hardware-test workload. Nothing in the kernel depends on this crate.
#![no_std]

#[cfg(feature = "runtime")]
pub mod discovery;

#[cfg(any(test, feature = "runtime"))]
mod frame;
#[cfg(any(test, feature = "runtime"))]
const MAX_FRAME_SIZE: usize = 125;

#[cfg(feature = "runtime")]
mod probe;
#[cfg(any(test, feature = "runtime"))]
mod statistics;
#[cfg(feature = "runtime")]
pub use probe::start;
#[cfg(feature = "runtime")]
pub use statistics::{statistics, Activity, Statistics};

#[cfg(feature = "runtime")]
const _: () = assert!(MAX_FRAME_SIZE == lattice_vertex::kernel::radio::MAX_FRAME_SIZE);
