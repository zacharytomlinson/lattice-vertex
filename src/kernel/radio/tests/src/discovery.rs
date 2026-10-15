//! Passive counters for normal kernel discovery. No packet generation/handling.
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering::Relaxed};
use lattice_vertex::kernel::radio::testing::{self, Event};

static REQUESTS_SENT: AtomicU32 = AtomicU32::new(0);
static RESPONSES_SENT: AtomicU32 = AtomicU32::new(0);
static REQUESTS_RECEIVED: AtomicU32 = AtomicU32::new(0);
static RESPONSES_RECEIVED: AtomicU32 = AtomicU32::new(0);
static IGNORED: AtomicU32 = AtomicU32::new(0);
static RX_ERRORS: AtomicU32 = AtomicU32::new(0);
static TX_ERRORS: AtomicU32 = AtomicU32::new(0);
static LOCAL_LOW: AtomicU32 = AtomicU32::new(0);
static LOCAL_HIGH: AtomicU32 = AtomicU32::new(0);
static PEER_LOW: AtomicU32 = AtomicU32::new(0);
static PEER_HIGH: AtomicU32 = AtomicU32::new(0);
static CHANNEL: AtomicU32 = AtomicU32::new(0);
static PAN: AtomicU32 = AtomicU32::new(0);
static FOUNDER_LOW: AtomicU32 = AtomicU32::new(0);
static FOUNDER_HIGH: AtomicU32 = AtomicU32::new(0);
static JOINED: AtomicU32 = AtomicU32::new(0);
static PROPOSALS: AtomicU32 = AtomicU32::new(0);
static AGREEMENT_CHANNEL: AtomicU32 = AtomicU32::new(0);
static SCAN_ERRORS: AtomicU32 = AtomicU32::new(0);
static ENERGY: [AtomicI32; 16] = [const { AtomicI32::new(i32::MIN) }; 16];
static OWNER_QUERIES_SENT: AtomicU32 = AtomicU32::new(0);
static OWNER_QUERIES_RECEIVED: AtomicU32 = AtomicU32::new(0);
static OWNER_REPLIES_SENT: AtomicU32 = AtomicU32::new(0);
static ARRIVALS_FORWARDED: AtomicU32 = AtomicU32::new(0);
static OWNER_NOTICES: AtomicU32 = AtomicU32::new(0);
static OWNER_LOW: AtomicU32 = AtomicU32::new(0);
static OWNER_HIGH: AtomicU32 = AtomicU32::new(0);
static OWNER_VIA_LOW: AtomicU32 = AtomicU32::new(0);
static OWNER_VIA_HIGH: AtomicU32 = AtomicU32::new(0);
static OWNER_BYTES: AtomicU32 = AtomicU32::new(0);
static OWNER_HOPS: AtomicU32 = AtomicU32::new(0);
static OWNER_SOLICITED: AtomicU32 = AtomicU32::new(0);
static LAST_SIGNAL: AtomicI32 = AtomicI32::new(i32::MIN);
static PEER_SIGNAL: AtomicI32 = AtomicI32::new(i32::MIN);

pub(super) fn last_signal() -> Option<i8> {
    match LAST_SIGNAL.load(Relaxed) {
        i32::MIN => None,
        value => Some(value as i8),
    }
}

#[derive(Debug)]
pub struct Statistics {
    pub local: u64,
    pub peer: u64,
    /// RSSI of the last directed presence frame from `peer`, not scan energy.
    pub peer_rssi_dbm: Option<i8>,
    /// Last compact owner notice, which may have crossed relays. No owner catalog.
    pub owner: u64,
    pub owner_via: u64,
    pub owner_surface_bytes: u32,
    pub owner_hops: u32,
    pub owner_solicited: bool,
    /// Notice count, not a count of distinct owners or a complete enumeration.
    pub owner_notices: u32,
    pub owner_queries_sent: u32,
    pub owner_queries_received: u32,
    pub owner_replies_sent: u32,
    pub arrivals_forwarded: u32,
    pub channel: u32,
    pub pan: u32,
    pub founder: u64,
    pub joined: bool,
    pub proposals_sent: u32,
    pub agreement_channel: u32,
    pub scan_errors: u32,
    /// Highest successful scan energy per channel, ordered 11 through 26.
    pub energy_dbm: [Option<i8>; 16],
    pub requests_sent: u32,
    pub responses_sent: u32,
    pub requests_received: u32,
    pub responses_received: u32,
    pub ignored: u32,
    pub receive_errors: u32,
    pub transmit_errors: u32,
}

/// Register before kernel startup. The kernel retains its ordinary handler.
pub fn observe() {
    testing::observe(event);
}

fn event(event: Event) {
    match event {
        Event::ReceiveSignal(dbm) => {
            LAST_SIGNAL.store(dbm.map_or(i32::MIN, i32::from), Relaxed);
        }
        Event::DiscoveryStarted {
            address,
            channel,
            pan_id,
        } => {
            LOCAL_LOW.store(address as u32, Relaxed);
            LOCAL_HIGH.store((address >> 32) as u32, Relaxed);
            CHANNEL.store(channel as u32, Relaxed);
            PAN.store(pan_id as u32, Relaxed);
        }
        Event::DiscoveryRequestSent => {
            REQUESTS_SENT.fetch_add(1, Relaxed);
        }
        Event::ChannelSample { channel, dbm } => {
            if let Some(dbm) = dbm {
                ENERGY[(channel - 11) as usize].fetch_max(dbm as i32, Relaxed);
            } else {
                SCAN_ERRORS.fetch_add(1, Relaxed);
            }
        }
        Event::ChannelProposalSent => {
            PROPOSALS.fetch_add(1, Relaxed);
        }
        Event::ProposalAgreed { channel } => {
            AGREEMENT_CHANNEL.store(channel as u32, Relaxed);
        }
        Event::NetworkSelected {
            founder,
            channel,
            joined,
        } => {
            FOUNDER_LOW.store(founder as u32, Relaxed);
            FOUNDER_HIGH.store((founder >> 32) as u32, Relaxed);
            CHANNEL.store(channel as u32, Relaxed);
            JOINED.store(joined as u32, Relaxed);
        }
        Event::DiscoveryResponseSent => {
            RESPONSES_SENT.fetch_add(1, Relaxed);
        }
        Event::DiscoveryRequestReceived(_) => {
            REQUESTS_RECEIVED.fetch_add(1, Relaxed);
        }
        Event::DiscoveryResponseReceived(peer) => {
            PEER_LOW.store(peer as u32, Relaxed);
            PEER_HIGH.store((peer >> 32) as u32, Relaxed);
            PEER_SIGNAL.store(LAST_SIGNAL.load(Relaxed), Relaxed);
            RESPONSES_RECEIVED.fetch_add(1, Relaxed);
        }
        Event::OwnerDiscoverySent => {
            OWNER_QUERIES_SENT.fetch_add(1, Relaxed);
        }
        Event::OwnerDiscoveryReceived => {
            OWNER_QUERIES_RECEIVED.fetch_add(1, Relaxed);
        }
        Event::OwnerResponseSent => {
            OWNER_REPLIES_SENT.fetch_add(1, Relaxed);
        }
        Event::ArrivalForwarded => {
            ARRIVALS_FORWARDED.fetch_add(1, Relaxed);
        }
        Event::OwnerObserved {
            owner,
            surface_bytes,
            via,
            hops,
            solicited,
        } => {
            OWNER_LOW.store(owner as u32, Relaxed);
            OWNER_HIGH.store((owner >> 32) as u32, Relaxed);
            OWNER_VIA_LOW.store(via as u32, Relaxed);
            OWNER_VIA_HIGH.store((via >> 32) as u32, Relaxed);
            OWNER_BYTES.store(surface_bytes as u32, Relaxed);
            OWNER_HOPS.store(hops as u32, Relaxed);
            OWNER_SOLICITED.store(solicited as u32, Relaxed);
            OWNER_NOTICES.fetch_add(1, Relaxed);
        }
        Event::FrameIgnored => {
            IGNORED.fetch_add(1, Relaxed);
        }
        Event::ReceiveFailed => {
            RX_ERRORS.fetch_add(1, Relaxed);
        }
        Event::TransmitFailed => {
            TX_ERRORS.fetch_add(1, Relaxed);
        }
        other => super::surface::event(other),
    }
}

/// Copy before printing. Prevent the resident task from preempting the reporter
/// between the two halves of an address; neither MCU supplies AtomicU64.
pub fn statistics(context: &lattice_vertex::sync::TaskContext) -> Statistics {
    let _ = context;
    lattice_vertex::port::portENTER_CRITICAL();
    let result = Statistics {
        local: (LOCAL_HIGH.load(Relaxed) as u64) << 32 | LOCAL_LOW.load(Relaxed) as u64,
        peer: (PEER_HIGH.load(Relaxed) as u64) << 32 | PEER_LOW.load(Relaxed) as u64,
        peer_rssi_dbm: match PEER_SIGNAL.load(Relaxed) {
            i32::MIN => None,
            value => Some(value as i8),
        },
        owner: (OWNER_HIGH.load(Relaxed) as u64) << 32 | OWNER_LOW.load(Relaxed) as u64,
        owner_via: (OWNER_VIA_HIGH.load(Relaxed) as u64) << 32 | OWNER_VIA_LOW.load(Relaxed) as u64,
        owner_surface_bytes: OWNER_BYTES.load(Relaxed),
        owner_hops: OWNER_HOPS.load(Relaxed),
        owner_solicited: OWNER_SOLICITED.load(Relaxed) != 0,
        owner_notices: OWNER_NOTICES.load(Relaxed),
        owner_queries_sent: OWNER_QUERIES_SENT.load(Relaxed),
        owner_queries_received: OWNER_QUERIES_RECEIVED.load(Relaxed),
        owner_replies_sent: OWNER_REPLIES_SENT.load(Relaxed),
        arrivals_forwarded: ARRIVALS_FORWARDED.load(Relaxed),
        channel: CHANNEL.load(Relaxed),
        pan: PAN.load(Relaxed),
        founder: (FOUNDER_HIGH.load(Relaxed) as u64) << 32 | FOUNDER_LOW.load(Relaxed) as u64,
        joined: JOINED.load(Relaxed) != 0,
        proposals_sent: PROPOSALS.load(Relaxed),
        agreement_channel: AGREEMENT_CHANNEL.load(Relaxed),
        scan_errors: SCAN_ERRORS.load(Relaxed),
        energy_dbm: core::array::from_fn(|i| match ENERGY[i].load(Relaxed) {
            i32::MIN => None,
            value => Some(value as i8),
        }),
        requests_sent: REQUESTS_SENT.load(Relaxed),
        responses_sent: RESPONSES_SENT.load(Relaxed),
        requests_received: REQUESTS_RECEIVED.load(Relaxed),
        responses_received: RESPONSES_RECEIVED.load(Relaxed),
        ignored: IGNORED.load(Relaxed),
        receive_errors: RX_ERRORS.load(Relaxed),
        transmit_errors: TX_ERRORS.load(Relaxed),
    };
    lattice_vertex::port::portEXIT_CRITICAL();
    result
}
