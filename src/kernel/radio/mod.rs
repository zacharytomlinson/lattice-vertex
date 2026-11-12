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

pub(crate) mod receive;

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

pub(crate) mod channel;

/// A consumed command retains its reply in the peer owner, not the raw RX queue.
#[derive(Clone, Copy)]
pub(crate) enum Reception {
    Frame(usize),
    Handled,
}
static IMMEDIATE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub(crate) struct ReceiveOrder;
impl ReceiveOrder {
    pub(crate) fn enter(empty: bool) -> Self {
        IMMEDIATE.store(empty, core::sync::atomic::Ordering::Release);
        Self
    }
}
impl Drop for ReceiveOrder {
    fn drop(&mut self) {
        IMMEDIATE.store(false, core::sync::atomic::Ordering::Release);
    }
}
pub(crate) fn receive_immediate(frame: &[u8]) -> bool {
    if !IMMEDIATE.load(core::sync::atomic::Ordering::Acquire) {
        return false;
    }
    if mac::is_ack(frame) {
        return false;
    }
    #[cfg(feature = "use-mutexes")]
    let handled = crate::kernel::peer::interrupt::receive(frame);
    #[cfg(not(feature = "use-mutexes"))]
    let handled = false;
    if !handled {
        IMMEDIATE.store(false, core::sync::atomic::Ordering::Release);
    }
    handled
}
