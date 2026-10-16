//! Bounded one-hop delivery. A retry preserves every frame byte and its DSN;
//! acknowledgement means frame reception, never appliance execution.

/// A missing ACK cannot cancel an application reply that may already be on air.
pub(crate) fn delivery_possible(result: Result<(), super::Error>) -> bool {
    matches!(result, Ok(()) | Err(super::Error::NoAcknowledgement))
}

pub(crate) const ATTEMPTS: u8 = 4;
pub(crate) const ACK_WAIT_US: u32 = 864; // 54 symbols at 16 us.
const RECENT_SENDERS: usize = 8;
const DUPLICATE_MS: u32 = 200;

/// The kernel's 2006 data profile, with an extended unicast destination.
pub(crate) fn requests_ack(frame: &[u8]) -> bool {
    frame.len() >= 21 && frame[0] == 0x61 && frame[1] == 0xdc
}

pub(crate) fn ack_for(frame: &[u8], pan: u16, local: u64) -> Option<u8> {
    (requests_ack(frame) && frame[3..5] == pan.to_le_bytes() && frame[5..13] == local.to_le_bytes())
        .then(|| frame[2])
}

pub(crate) fn is_ack(frame: &[u8]) -> bool {
    frame.len() == 3 && frame[0] & !0x10 == 2 && frame[1] == 0
}

pub(crate) fn matches_ack(frame: &[u8], sequence: u8) -> bool {
    is_ack(frame) && frame[2] == sequence
}

/// In-flight RX + full TX frame + ACK, rounded up with a tick-boundary allowance. Ports retain
/// DMA storage until completion or synchronous cancellation on deadline.
pub(crate) fn attempt_ticks(tick_hz: u32) -> u32 {
    let us = (super::MAX_FRAME_SIZE as u64 + 8) * 64 + u64::from(ACK_WAIT_US);
    (us * u64::from(tick_hz)).div_ceil(1_000_000) as u32 + 2
}

pub(crate) struct Backoff(u32);
impl Backoff {
    pub(crate) fn new(address: u64) -> Self {
        Self((address as u32 ^ (address >> 32) as u32) | 1)
    }
    /// Retry delay: randomized 320-us slots, rounded up to kernel ticks.
    pub(crate) fn ticks(&mut self, retry: u8, tick_hz: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        let slots = self.0 & ((1 << (3 + retry.min(2))) - 1);
        (u64::from(slots) * 320 * u64::from(tick_hz)).div_ceil(1_000_000) as u32
    }
}

#[derive(Clone, Copy)]
struct Received {
    source: u64,
    sequence: u8,
    at_ms: u32,
}

/// Last accepted DSN per sender. A sender completes its bounded retries before
/// starting another frame. Eight recent senders; oldest replacement and expiry
/// are explicit. This is not persistent exactly-once execution history.
pub(crate) struct Duplicates([Option<Received>; RECENT_SENDERS]);
impl Duplicates {
    pub(crate) const fn new() -> Self {
        Self([None; RECENT_SENDERS])
    }
    pub(crate) fn accept(&mut self, frame: &[u8], now_ms: u32) -> bool {
        if is_ack(frame) {
            return false;
        }
        if !requests_ack(frame) {
            return true;
        }
        let source = u64::from_le_bytes(frame[13..21].try_into().unwrap());
        let sequence = frame[2];
        if let Some(at) = self
            .0
            .iter()
            .position(|r| r.is_some_and(|r| r.source == source))
        {
            let old = self.0[at].unwrap();
            if old.sequence == sequence && now_ms.wrapping_sub(old.at_ms) < DUPLICATE_MS {
                return false;
            }
            self.0[at] = Some(Received {
                source,
                sequence,
                at_ms: now_ms,
            });
        } else {
            let at = self.0.iter().position(Option::is_none).unwrap_or_else(|| {
                (0..RECENT_SENDERS)
                    .max_by_key(|&i| now_ms.wrapping_sub(self.0[i].unwrap().at_ms))
                    .unwrap()
            });
            self.0[at] = Some(Received {
                source,
                sequence,
                at_ms: now_ms,
            });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(source: u64, seq: u8) -> [u8; 21] {
        let mut f = [0; 21];
        f[..3].copy_from_slice(&[0x61, 0xdc, seq]);
        f[3..5].copy_from_slice(&0x1234u16.to_le_bytes());
        f[5..13].copy_from_slice(&42u64.to_le_bytes());
        f[13..21].copy_from_slice(&source.to_le_bytes());
        f
    }
    #[test]
    fn acknowledgement_targets_and_duplicate_delivery() {
        let mut seen = Duplicates::new();
        let f = frame(7, 255);
        assert_eq!(ack_for(&f, 0x1234, 42), Some(255));
        assert_eq!(ack_for(&f, 0x1235, 42), None);
        assert_eq!(ack_for(&f, 0x1234, 43), None);
        for short in 0..21 {
            assert_eq!(ack_for(&f[..short], 0x1234, 42), None);
        }
        // A lost first ACK causes a retransmission, not a second delivery.
        for (at, accepted) in [(0, true), (7, false), (20, false), (45, false)] {
            assert_eq!(
                ack_for(&f, 0x1234, 42),
                Some(255),
                "duplicates still need ACKs"
            );
            assert_eq!(seen.accept(&f, at), accepted);
        }
        assert!(
            seen.accept(&frame(8, 255), 46),
            "another sender owns its DSN"
        );
        assert!(
            seen.accept(&frame(7, 0), 47),
            "sequence wrap is a new frame"
        );
        assert!(
            seen.accept(&f, 48),
            "intervening accepted sequence replaces history"
        );
        assert!(seen.accept(&f, 248), "bounded history expires");
        for ack in [[2, 0, 255], [0x12, 0, 255]] {
            assert!(matches_ack(&ack, 255));
            assert!(!seen.accept(&ack, 249));
        }
        for ack in [[2, 0, 0], [0x22, 0, 255], [2, 0x10, 255]] {
            assert!(!matches_ack(&ack, 255));
        }
        let mut broadcast = f;
        broadcast[0] = 0x41;
        broadcast[1] = 0xd8;
        assert!(!requests_ack(&broadcast));
        assert!(seen.accept(&broadcast, 250));
        assert!(seen.accept(&broadcast, 251));
    }
    #[test]
    fn bounded_history_wrap_and_retry_budget() {
        let mut seen = Duplicates::new();
        for n in 1..=9 {
            assert!(seen.accept(&frame(n, 3), u32::MAX - 20 + n as u32));
        }
        assert!(seen.accept(&frame(1, 3), 0), "oldest sender evicted");
        assert!(
            !seen.accept(&frame(9, 3), 1),
            "time wrap does not expire recent data"
        );
        assert!(seen.accept(&frame(9, 3), 200));
        for (result, possible) in [
            (Ok(()), true),
            (Err(super::super::Error::NoAcknowledgement), true),
            (Err(super::super::Error::ChannelBusy), false),
            (Err(super::super::Error::InvalidFrame), false),
            (Err(super::super::Error::Aborted), false),
        ] {
            assert_eq!(delivery_possible(result), possible);
        }
        let mut backoff = Backoff::new(42);
        for retry in 0..3 {
            for _ in 0..32 {
                assert!(backoff.ticks(retry, 1000) <= 10);
            }
        }
        assert_eq!(attempt_ticks(1000), 12);
    }
}
