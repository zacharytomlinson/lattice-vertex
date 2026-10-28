//! Frames captured during receive opportunities and channel-access backoff.
//! These are owned wire bytes, awaiting ordinary peer validation and dispatch.
use super::MAX_FRAME_SIZE;

const CAPACITY: usize = 4;

/// Broadcast pressure must not evict a frame the peer sent with a MAC ACK request.
/// If every retained frame is addressed, discard the newcomer instead.
pub(crate) fn oldest_best_effort<'a>(mut frames: impl Iterator<Item = &'a [u8]>) -> Option<usize> {
    frames.position(|frame| !super::mac::requests_ack(frame))
}

pub(crate) struct ReceiveBuffer {
    frames: [[u8; MAX_FRAME_SIZE]; CAPACITY],
    lengths: [u8; CAPACITY],
    first: usize,
    count: usize,
}
impl ReceiveBuffer {
    pub(super) fn pending(&self) -> bool {
        self.count != 0
    }
    pub(super) const fn new() -> Self {
        Self {
            frames: [[0; MAX_FRAME_SIZE]; CAPACITY],
            lengths: [0; CAPACITY],
            first: 0,
            count: 0,
        }
    }
    /// Retain FIFO order, replacing the oldest best-effort frame under pressure.
    /// Return whether any frame was dropped, including a refused newcomer.
    pub(crate) fn push(&mut self, frame: &[u8]) -> bool {
        assert!(frame.len() <= MAX_FRAME_SIZE);
        let evicted = self.count == CAPACITY;
        if evicted {
            let Some(remove) = oldest_best_effort((0..self.count).map(|i| {
                let at = (self.first + i) % CAPACITY;
                &self.frames[at][..self.lengths[at] as usize]
            })) else {
                return true;
            };
            for i in remove..self.count - 1 {
                let at = (self.first + i) % CAPACITY;
                let next = (at + 1) % CAPACITY;
                self.frames[at] = self.frames[next];
                self.lengths[at] = self.lengths[next];
            }
            self.count -= 1;
        }
        let at = (self.first + self.count) % CAPACITY;
        self.frames[at][..frame.len()].copy_from_slice(frame);
        self.lengths[at] = frame.len() as u8;
        self.count += 1;
        evicted
    }
    pub(super) fn pop(&mut self, out: &mut [u8; MAX_FRAME_SIZE]) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let length = self.lengths[self.first] as usize;
        out[..length].copy_from_slice(&self.frames[self.first][..length]);
        self.first = (self.first + 1) % CAPACITY;
        self.count -= 1;
        Some(length)
    }
    pub(super) fn clear(&mut self) {
        self.first = 0;
        self.count = 0;
    }
}

/// One maximum 250-kbit/s frame (MAC + FCS + PHY header/preamble) and 192 us
/// turnaround. The extra tick covers starting just before a tick boundary.
pub(crate) const fn receive_turn_ticks(tick_hz: u32) -> u32 {
    const WINDOW_US: u64 = (MAX_FRAME_SIZE as u64 + 2 + 6) * 32 + 192;
    (WINDOW_US * tick_hz as u64).div_ceil(1_000_000) as u32 + 1
}

/// Once interruption is requested, allow a frame already on air to finish.
/// A stuck hardware indication cannot retain the radio task indefinitely.
pub(super) fn finish_frame_ticks(
    started: &mut Option<u32>,
    now: u32,
    receiving: bool,
    limit: u32,
) -> Option<u32> {
    if !receiving {
        return None;
    }
    let since = *started.get_or_insert(now);
    let remaining = limit.saturating_sub(now.wrapping_sub(since));
    (remaining != 0).then_some(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_frames_survive_dma_buffer_reuse_and_evict_only_the_oldest() {
        let mut buffer = ReceiveBuffer::new();
        let mut dma = [0; MAX_FRAME_SIZE];
        // Include maximum frames and short frames: stale suffixes are not data.
        let lengths = [125, 31, 85, 1, 67, 125];
        for (i, length) in lengths.into_iter().enumerate() {
            dma.fill(i as u8);
            assert_eq!(buffer.push(&dma[..length]), i >= CAPACITY);
        }
        dma.fill(255);
        for (i, length) in lengths.into_iter().enumerate().skip(2) {
            assert_eq!(buffer.pop(&mut dma), Some(length));
            assert!(dma[..length].iter().all(|byte| *byte == i as u8));
        }
        assert_eq!(buffer.pop(&mut dma), None);
        buffer.push(&[42]);
        buffer.clear();
        assert_eq!(
            buffer.pop(&mut dma),
            None,
            "channel changes discard old-channel frames"
        );
        assert!(!buffer.push(&[7, 8]));
        assert_eq!(buffer.pop(&mut dma), Some(2));
        assert_eq!(&dma[..2], &[7, 8]);
    }

    #[test]
    fn addressed_retention_and_fifo() {
        fn frame(id: u8, addressed: bool) -> [u8; 22] {
            let mut bytes = [0; 22];
            bytes[..2].copy_from_slice(if addressed {
                &[0x61, 0xdc]
            } else {
                &[0x41, 0xd8]
            });
            bytes[21] = id;
            bytes
        }
        for first in 0..CAPACITY {
            for mask in 0u8..16 {
                for incoming in [false, true] {
                    let mut buffer = ReceiveBuffer::new();
                    let mut out = [0; MAX_FRAME_SIZE];
                    for _ in 0..first {
                        buffer.push(&frame(9, false));
                        buffer.pop(&mut out);
                    }
                    for i in 0..CAPACITY {
                        assert!(!buffer.push(&frame(i as u8, mask & (1 << i) != 0)));
                    }
                    assert!(buffer.push(&frame(4, incoming)));
                    let removed = (0..CAPACITY).find(|i| mask & (1 << i) == 0);
                    for i in 0..=CAPACITY {
                        if removed == Some(i) || (i == CAPACITY && removed.is_none()) {
                            continue;
                        }
                        assert_eq!(buffer.pop(&mut out), Some(22));
                        assert_eq!(out[21], i as u8);
                    }
                    assert_eq!(buffer.pop(&mut out), None);
                }
            }
        }
    }

    #[test]
    fn receive_turn_covers_a_maximum_frame_even_at_a_tick_boundary() {
        for hz in [100, 1_000, 2_000, 32_768] {
            let ticks = receive_turn_ticks(hz);
            let shortest_us = u64::from(ticks - 1) * 1_000_000 / u64::from(hz);
            assert!(shortest_us >= 4_448);
        }
        assert_eq!(receive_turn_ticks(1_000), 6);
    }
    #[test]
    fn interruption_finishes_an_active_frame_but_bounds_a_stuck_receiver() {
        let mut started = None;
        assert_eq!(finish_frame_ticks(&mut started, 10, false, 6), None);
        assert_eq!(started, None, "idle RX needs no extension");
        assert_eq!(
            finish_frame_ticks(&mut started, u32::MAX - 2, true, 6),
            Some(6)
        );
        assert_eq!(finish_frame_ticks(&mut started, 0, true, 6), Some(3));
        assert_eq!(finish_frame_ticks(&mut started, 1, false, 6), None);
        assert_eq!(
            finish_frame_ticks(&mut started, 3, true, 6),
            None,
            "new wakeups must not restart the frame-completion deadline"
        );
    }
}
