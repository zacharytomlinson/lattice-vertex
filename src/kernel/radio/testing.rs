//! Test-only entry to the resident transport task. No probe definitions live here.

pub use super::task::Transport;
use crate::{kernel, port::RadioPeripheral, sync::TaskContext};

#[derive(Clone, Copy)]
pub enum Event {
    NotificationWait,
    InterruptWake,
    ReceiveEvicted,
    /// Signal strength reported by a port that supplies RSSI metadata.
    ReceiveSignal(i8),
    /// Port-native RX-abort reason reported by the hardware interrupt.
    /// This reports observed events; it does not change hardware interrupt masks.
    ReceiveAborted(u32),
    DiscoveryStarted {
        address: u64,
        channel: u8,
        pan_id: u16,
    },
    DiscoveryRequestSent,
    DiscoveryResponseSent,
    DiscoveryRequestReceived(u64),
    DiscoveryResponseReceived(u64),
    ChannelSample {
        channel: u8,
        dbm: Option<i8>,
    },
    ChannelProposalSent,
    ProposalAgreed {
        channel: u8,
    },
    NetworkSelected {
        founder: u64,
        channel: u8,
        joined: bool,
    },
    FrameIgnored,
    ReceiveFailed,
    TransmitFailed,
}

static mut WORKLOAD: Option<fn(&mut Transport) -> !> = None;
static mut EVENTS: Option<fn(Event)> = None;

/// Observe the normal kernel path without installing a replacement workload.
/// Register before scheduler startup. The callback must be short/nonblocking;
/// radio events can arrive in interrupt context. All storage belongs to the test.
pub fn observe(events: fn(Event)) {
    assert_eq!(
        kernel::tasks::xTaskGetSchedulerState(),
        kernel::tasks::taskSCHEDULER_NOT_STARTED
    );
    unsafe {
        assert!(EVENTS.is_none(), "hardware observer already installed");
        EVENTS = Some(events);
    }
}

/// Install a hardware-test workload and enter normal kernel startup.
///
/// `workload` runs in the resident radio task and never returns. `events` must
/// be short and nonblocking: it can run in a radio ISR with interrupts masked.
/// The fixture owns all packet definitions, traffic policy, and counters.
pub fn start(
    context: TaskContext,
    radio: RadioPeripheral,
    workload: fn(&mut Transport) -> !,
    events: fn(Event),
) -> ! {
    assert_eq!(
        kernel::tasks::xTaskGetSchedulerState(),
        kernel::tasks::taskSCHEDULER_NOT_STARTED
    );
    unsafe {
        assert!(WORKLOAD.is_none(), "hardware test already installed");
        WORKLOAD = Some(workload);
    }
    observe(events);
    kernel::start(context, radio)
}

pub(super) fn run(transport: &mut Transport) {
    // Initialized once before scheduler startup; immutable while tasks/IRQs run.
    if let Some(workload) = unsafe { WORKLOAD } {
        workload(transport);
    }
}

pub(crate) fn event(event: Event) {
    if let Some(handler) = unsafe { EVENTS } {
        handler(event);
    }
}
