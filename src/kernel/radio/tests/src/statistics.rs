//! Counters owned by the hardware test workload.
#![cfg_attr(not(feature = "runtime"), allow(dead_code))]

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU8, Ordering};

/// Operation being attempted by the resident task, not a PHY status register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Activity {
    Initializing,
    Receiving,
    Transmitting,
}

/// A coherent snapshot is not implied across fields; each field is atomic.
#[derive(Debug, Clone, Copy)]
pub struct Statistics {
    pub activity: Activity,
    /// Entries into the task-notification wait after a pending driver poll.
    pub notification_waits: u32,
    /// Radio driver wakers invoked from an interrupt handler.
    pub interrupt_wakes: u32,
    /// Receive deadlines reached by the diagnostic probe originator.
    pub receive_timeouts: u32,
    pub received: u32,
    pub transmitted: u32,
    pub replies_received: u32,
    pub replies_sent: u32,
    pub ignored: u32,
    pub receive_errors: u32,
    pub transmit_errors: u32,
    pub evicted: u32,
    pub probes_sent: u32,
    pub pre_peer_probes: u32,
    /// Completed reply windows for probes sent after a peer was observed.
    pub unanswered: u32,
    pub pending_probe: Option<u32>,
    pub unmatched_replies: u32,
    pub rssi_dbm: Option<i8>,
    /// IRQ-reported receive aborts; does not count every possible RF loss.
    pub rx_abort_events: u32,
    pub rx_abort_last: u32,
}

pub(super) static ACTIVITY: AtomicU8 = AtomicU8::new(Activity::Initializing as u8);
pub(super) static NOTIFICATION_WAITS: AtomicU32 = AtomicU32::new(0);
pub(super) static INTERRUPT_WAKES: AtomicU32 = AtomicU32::new(0);
pub(super) static RECEIVE_TIMEOUTS: AtomicU32 = AtomicU32::new(0);
pub(super) static RECEIVED: AtomicU32 = AtomicU32::new(0);
pub(super) static TRANSMITTED: AtomicU32 = AtomicU32::new(0);
pub(super) static REPLIES_RECEIVED: AtomicU32 = AtomicU32::new(0);
pub(super) static REPLIES_SENT: AtomicU32 = AtomicU32::new(0);
pub(super) static IGNORED: AtomicU32 = AtomicU32::new(0);
pub(super) static RECEIVE_ERRORS: AtomicU32 = AtomicU32::new(0);
pub(super) static TRANSMIT_ERRORS: AtomicU32 = AtomicU32::new(0);
pub(super) static EVICTED: AtomicU32 = AtomicU32::new(0);
pub(super) static PROBES_SENT: AtomicU32 = AtomicU32::new(0);
pub(super) static PRE_PEER_PROBES: AtomicU32 = AtomicU32::new(0);
pub(super) static UNANSWERED: AtomicU32 = AtomicU32::new(0);
pub(super) static PENDING_PROBE: AtomicBool = AtomicBool::new(false);
pub(super) static LAST_PROBE_TOKEN: AtomicU32 = AtomicU32::new(0);
pub(super) static UNMATCHED_REPLIES: AtomicU32 = AtomicU32::new(0);
pub(super) static RSSI: AtomicI32 = AtomicI32::new(i32::MIN);
pub(super) static RX_ABORT_EVENTS: AtomicU32 = AtomicU32::new(0);
pub(super) static RX_ABORT_LAST: AtomicU32 = AtomicU32::new(0);

/// One diagnostic reply window. Startup-only probes and the currently pending
/// probe are kept out of the unanswered count. It never schedules a retry.
#[derive(Default)]
pub(super) struct ReplyWindow {
    peer_seen: bool,
    pending: Option<(u32, bool)>,
}

impl ReplyWindow {
    pub fn observe_peer(&mut self) {
        self.peer_seen = true;
    }

    /// Open a window only after a successful send. Return whether a peer had
    /// already been observed when sending this probe.
    pub fn sent(&mut self, token: u32) -> bool {
        debug_assert!(self.pending.is_none());
        self.pending = Some((token, self.peer_seen));
        self.peer_seen
    }

    pub fn reply(&mut self, token: u32) -> bool {
        if self.pending.is_some_and(|(expected, _)| expected == token) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    /// Called at the next probe deadline. An eligible, unanswered window counts
    /// once; a late/duplicate reply cannot erase it or answer the next probe.
    pub fn close(&mut self) -> bool {
        self.pending.take().is_some_and(|(_, eligible)| eligible)
    }
}

/// Read diagnostic counters without accessing the radio or driving its loop.
pub fn statistics() -> Statistics {
    Statistics {
        activity: match ACTIVITY.load(Ordering::Relaxed) {
            1 => Activity::Receiving,
            2 => Activity::Transmitting,
            _ => Activity::Initializing,
        },
        notification_waits: NOTIFICATION_WAITS.load(Ordering::Relaxed),
        interrupt_wakes: INTERRUPT_WAKES.load(Ordering::Relaxed),
        receive_timeouts: RECEIVE_TIMEOUTS.load(Ordering::Relaxed),
        received: RECEIVED.load(Ordering::Relaxed),
        transmitted: TRANSMITTED.load(Ordering::Relaxed),
        replies_received: REPLIES_RECEIVED.load(Ordering::Relaxed),
        replies_sent: REPLIES_SENT.load(Ordering::Relaxed),
        ignored: IGNORED.load(Ordering::Relaxed),
        receive_errors: RECEIVE_ERRORS.load(Ordering::Relaxed),
        transmit_errors: TRANSMIT_ERRORS.load(Ordering::Relaxed),
        evicted: EVICTED.load(Ordering::Relaxed),
        probes_sent: PROBES_SENT.load(Ordering::Relaxed),
        pre_peer_probes: PRE_PEER_PROBES.load(Ordering::Relaxed),
        unanswered: UNANSWERED.load(Ordering::Relaxed),
        pending_probe: PENDING_PROBE
            .load(Ordering::Relaxed)
            .then(|| LAST_PROBE_TOKEN.load(Ordering::Relaxed)),
        unmatched_replies: UNMATCHED_REPLIES.load(Ordering::Relaxed),
        rssi_dbm: match RSSI.load(Ordering::Relaxed) {
            i32::MIN => None,
            value => Some(value as i8),
        },
        rx_abort_events: RX_ABORT_EVENTS.load(Ordering::Relaxed),
        rx_abort_last: RX_ABORT_LAST.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::ReplyWindow;

    #[test]
    fn only_closed_unanswered_windows_sent_after_peer_contact_count() {
        for (peer_before_send, reply_token, expected_unanswered) in [
            (false, None, false),
            (false, Some(7), false),
            (true, None, true),
            (true, Some(7), false),
            (true, Some(6), true),
        ] {
            let mut window = ReplyWindow::default();
            if peer_before_send {
                window.observe_peer();
            }
            assert_eq!(window.sent(7), peer_before_send);
            // Contact after sending must not turn an unpaired startup send into
            // an eligible loss sample.
            window.observe_peer();
            if let Some(token) = reply_token {
                assert_eq!(window.reply(token), token == 7);
            }
            assert_eq!(window.close(), expected_unanswered);
            assert!(!window.close(), "a window must not be counted twice");
            window.sent(8);
            assert!(!window.reply(7), "an old reply must not answer a new probe");
            assert!(window.reply(8));
            assert!(!window.reply(8), "duplicate reply");
            assert!(!window.close());
        }
    }
}
