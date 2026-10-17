//! Normalized, directional communication time. One unit is one MAC payload byte.
//! Fixed-point values make costs comparable across MCU clocks without floating point.

/// Estimated microseconds per acknowledged payload byte, with eight fractional
/// bits. MAX denotes unknown, not a free path. Zero is the empty/local path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimePerByte(u32);

impl TimePerByte {
    pub const UNKNOWN: Self = Self(u32::MAX);
    pub const ZERO: Self = Self(0);

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
    pub const fn raw(self) -> u32 {
        self.0
    }
    pub const fn is_known(self) -> bool {
        self.0 != u32::MAX
    }

    /// Normalize observed service time. Failed attempts contribute time but no
    /// acknowledged bytes. Rounding is upward; an unrepresentable cost is unknown.
    pub fn measured(elapsed_us: u32, bytes: u32) -> Self {
        if bytes == 0 || elapsed_us == 0 {
            return Self::UNKNOWN;
        }
        let value = (u64::from(elapsed_us) * 256).div_ceil(u64::from(bytes));
        if value >= u64::from(u32::MAX) {
            Self::UNKNOWN
        } else {
            Self(value as u32)
        }
    }

    /// Compose serial forwarding stages. Unknown or overflow stays unknown.
    pub fn then(self, following: Self) -> Self {
        if !self.is_known() || !following.is_known() {
            return Self::UNKNOWN;
        }
        self.0
            .checked_add(following.0)
            .filter(|&n| n != u32::MAX)
            .map_or(Self::UNKNOWN, Self)
    }

    /// Apply any information quantity to the same cost; there is no bulk mode.
    pub fn estimate_us(self, bytes: u32) -> Option<u64> {
        if bytes == 0 {
            return Some(0);
        }
        self.is_known()
            .then(|| (u64::from(self.0) * u64::from(bytes)).div_ceil(256))
    }

    /// Relative cost as an exact numerator/denominator pair in the common scale.
    pub fn ratio(self, other: Self) -> Option<(u32, u32)> {
        (self.is_known() && other.is_known() && other.0 != 0).then_some((self.0, other.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_cost_orders_composes_and_applies_quantity() {
        let a = TimePerByte::measured(2_000, 100);
        let same = TimePerByte::measured(8_000, 400);
        let b = TimePerByte::measured(3_000, 100);
        assert_eq!(a, same);
        assert!(a < b);
        assert_eq!(a.ratio(b), Some((5120, 7680)));
        for bytes in [10, 10_000, u32::MAX] {
            assert_eq!(a.estimate_us(bytes), Some(u64::from(bytes) * 20));
            assert_eq!(a.then(b).estimate_us(bytes), Some(u64::from(bytes) * 50));
        }
        assert_eq!(a.then(TimePerByte::ZERO), a);
        for unknown in [
            TimePerByte::UNKNOWN,
            TimePerByte::measured(0, 10),
            TimePerByte::measured(10, 0),
            TimePerByte::measured(u32::MAX, 1),
        ] {
            assert_eq!(unknown.estimate_us(1), None);
            assert_eq!(a.then(unknown), TimePerByte::UNKNOWN);
            assert!(unknown > a);
            assert_eq!(a.ratio(unknown), None);
        }
        assert_eq!(a.ratio(TimePerByte::ZERO), None);
        assert_eq!(TimePerByte::UNKNOWN.estimate_us(0), Some(0));
        assert_eq!(
            TimePerByte::from_raw(u32::MAX - 1).then(a),
            TimePerByte::UNKNOWN
        );
        assert_eq!(TimePerByte::measured(1, 3).estimate_us(3), Some(2));
    }
}
