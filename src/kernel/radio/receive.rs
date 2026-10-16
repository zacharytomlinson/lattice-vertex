//! Frames captured while a peer gives its neighbors a receive turn after TX.
//! These are owned wire bytes, awaiting ordinary peer validation and dispatch.
use super::MAX_FRAME_SIZE;

const CAPACITY: usize = 4;

pub(crate) struct ReceiveBuffer {
    frames: [[u8; MAX_FRAME_SIZE]; CAPACITY],
    lengths: [u8; CAPACITY],
    first: usize,
    count: usize,
}
impl ReceiveBuffer {
    pub(super) const fn new() -> Self {
        Self {
            frames: [[0; MAX_FRAME_SIZE]; CAPACITY],
            lengths: [0; CAPACITY],
            first: 0,
            count: 0,
        }
    }
    /// Keep the latest frames under pressure. Return whether the oldest was evicted.
    pub(crate) fn push(&mut self, frame: &[u8]) -> bool {
        assert!(frame.len() <= MAX_FRAME_SIZE);
        let evicted = self.count == CAPACITY;
        if evicted {
            self.first = (self.first + 1) % CAPACITY;
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
pub(super) const fn receive_turn_ticks(tick_hz: u32) -> u32 {
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
