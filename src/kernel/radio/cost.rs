//! Measurements handed from the resident transport to its adjacent-link records.
pub use lattice_peer_wire::cost::TimePerByte;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Transmission {
    pub peer: u64,
    pub elapsed_us: u32,
    /// Zero when no ACK confirmed delivery, even if the peer might have received it.
    pub acknowledged_bytes: u16,
}

/// Bounded recent observations, owned by the existing neighbor record. Includes
/// unsuccessful service time; idle time between sends and remote application
/// execution are excluded. Mandatory post-send receive time remains included.
#[derive(Clone, Copy)]
pub(crate) struct Measurement {
    elapsed_us: u32,
    bytes: u32,
}
impl Measurement {
    pub(crate) const EMPTY: Self = Self {
        elapsed_us: 0,
        bytes: 0,
    };
    pub(crate) fn observe(&mut self, sample: Transmission) {
        // Roughly 4 KiB of evidence, exponentially aged without a sample array.
        if self.bytes >= 4096 || self.elapsed_us > u32::MAX / 2 {
            self.elapsed_us = self.elapsed_us.div_ceil(2);
            self.bytes /= 2;
        }
        self.elapsed_us = self.elapsed_us.saturating_add(sample.elapsed_us);
        self.bytes += u32::from(sample.acknowledged_bytes);
    }
    pub(crate) fn cost(self) -> TimePerByte {
        TimePerByte::measured(self.elapsed_us, self.bytes)
    }
}

/// Temporary handoff only. The peer drains before dispatch and at each loop:
/// at most continuation + host + delta + maintenance sends, or two split replies
/// plus an update-absence notice, lie between drains.
pub(crate) struct Samples([Option<Transmission>; 4]);
impl Samples {
    pub(crate) const fn new() -> Self {
        Self([None; 4])
    }
    pub(crate) fn push(&mut self, value: Transmission) {
        if let Some(slot) = self.0.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(value);
        } else {
            // Measurements may be discarded; never manufacture delivery evidence.
            self.0.rotate_left(1);
            self.0[3] = Some(value);
        }
    }
    pub(crate) fn take(&mut self) -> Option<Transmission> {
        self.0
            .iter_mut()
            .find(|slot| slot.is_some())
            .and_then(Option::take)
    }
    pub(crate) fn clear(&mut self) {
        self.0.fill(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_attempts_cost_time_without_claiming_bytes_and_samples_age() {
        let mut m = Measurement::EMPTY;
        let mut sample = Transmission {
            peer: 7,
            elapsed_us: 1000,
            acknowledged_bytes: 0,
        };
        m.observe(sample);
        assert_eq!(m.cost(), TimePerByte::UNKNOWN);
        sample.acknowledged_bytes = 100;
        m.observe(sample);
        assert_eq!(m.cost().estimate_us(100), Some(2000));
        m.observe(Transmission {
            acknowledged_bytes: 0,
            ..sample
        });
        assert_eq!(m.cost().estimate_us(100), Some(3000));
        for _ in 0..1000 {
            m.observe(sample);
        }
        assert!(m.bytes <= 4195);
        assert!(m.cost().estimate_us(100).unwrap() < 1100);
        let mut samples = Samples::new();
        for peer in 1..=5 {
            samples.push(Transmission { peer, ..sample });
        }
        for peer in 2..=5 {
            assert_eq!(samples.take().unwrap().peer, peer);
        }
        assert!(samples.take().is_none());
        samples.push(sample);
        samples.clear();
        assert!(samples.take().is_none());
    }
}
