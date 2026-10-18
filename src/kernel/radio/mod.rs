//! Kernel-owned radio task, frame storage, and interrupt-driven transport.
//!
//! Ordinary startup hands the initialized transport to common peer discovery.
//! Hardware probes may replace that handler only through the test-support entry.

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(crate) mod task;
#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(crate) use task::start;

/// Access for hardware-test firmware, excluded from ordinary kernel builds.
#[cfg(all(
    feature = "test-support",
    any(feature = "port-nrf52840", feature = "port-esp32c6")
))]
#[doc(hidden)]
pub mod testing;

#[cfg(any(
    test,
    feature = "firmware-update",
    feature = "port-nrf52840",
    feature = "port-esp32c6"
))]
pub(crate) mod receive;

#[cfg(any(test, feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(crate) mod mac;

/// Maximum MAC frame size, excluding the hardware checksum.
pub const MAX_FRAME_SIZE: usize = 125;

/// Errors from the selected hardware port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidConfiguration,
    InvalidFrame,
    ChannelBusy,
    /// No matching link acknowledgement; peer execution remains unknown.
    NoAcknowledgement,
    Aborted,
}

/// Shared normalized communication cost and internal measurement support.
pub mod cost;
