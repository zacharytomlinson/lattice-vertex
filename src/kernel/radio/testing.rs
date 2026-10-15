//! Test-only entry to the resident transport task. No probe definitions live here.

pub use super::task::Transport;
use crate::{kernel, port::RadioPeripheral, sync::TaskContext};

/// Observations used by maintained kernel checks. Some arrive from an ISR;
/// callbacks must remain short and nonblocking. Borrowed data is not retained.
#[derive(Clone, Copy)]
pub enum Event<'a> {
    NotificationWait,
    InterruptWake,
    ReceiveEvicted,
    /// Current packet RSSI; None prevents retaining a previous packet's sample.
    ReceiveSignal(Option<i8>),
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
    InvocationSent,
    InvocationReceived,
    InvocationResultSent {
        status: u8,
    },
    StateChangeSent {
        owner: u64,
        attribute: u64,
        sequence: u32,
        value: Option<crate::ir::Value>,
        evicted: u32,
    },
    StateChangeReceived {
        owner: u64,
        attribute: u64,
        sequence: u32,
        value: Option<crate::ir::Value>,
    },
    ArrivalSent,
    ArrivalForwarded,
    ArrivalReceived,
    OwnerDiscoverySent,
    OwnerDiscoveryReceived,
    OwnerResponseSent,
    /// A compact owner announcement or matching discovery reply, not an IR
    /// copy or a guarantee of complete network enumeration. RSSI belongs to
    /// `via`, the immediate transmitter, not necessarily to `owner`.
    OwnerObserved {
        owner: u64,
        surface_bytes: u16,
        via: u64,
        hops: u8,
        solicited: bool,
    },
    SurfaceReadReceived {
        reader: u64,
        owner: u64,
        request: u16,
        offset: u16,
    },
    SurfaceReadForwarded,
    /// Borrowed range bytes, emitted only in the resident task after TX completes.
    SurfacePartSent {
        reader: u64,
        owner: u64,
        request: u16,
        offset: u16,
        total: u16,
        bytes: &'a [u8],
    },
    /// Borrowed packet bytes, emitted only in the resident task. No retention is implied.
    SurfacePartReceived {
        reader: u64,
        owner: u64,
        request: u16,
        offset: u16,
        total: u16,
        bytes: &'a [u8],
    },
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
    static APPLIANCE: crate::kernel::Appliance<(), core::convert::Infallible> =
        crate::kernel::Appliance::new(&crate::surface::EMPTY);
    APPLIANCE.initialize(&context, ());
    kernel::start(context, radio, &APPLIANCE)
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
