//! Commissioned coordinates and recurring civil-time rules. No timezone names or
//! host database travel to the MCU. Offsets are seconds east of UTC.
pub const PROFILE_BYTES: usize = 32;
pub const DAY: i64 = 86_400;
pub const MIN_UTC: i64 = 946_684_800; // 2000-01-01
pub const MAX_UTC: i64 = 253_402_300_800; // 10000-01-01, exclusive

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}
impl Date {
    pub fn days(self) -> i64 {
        let mut y = self.year as i64;
        let m = self.month as i64;
        y -= i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + self.day as i64 - 1;
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
    }
    pub fn from_days(days: i64) -> Self {
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let mut y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = mp + if mp < 10 { 3 } else { -9 };
        y += i64::from(m <= 2);
        Self {
            year: y as i32,
            month: m as u8,
            day: d as u8,
        }
    }
    pub fn at(utc: i64) -> Self {
        Self::from_days(utc.div_euclid(DAY))
    }
    pub fn ordinal(self) -> u16 {
        (self.days()
            - Self {
                year: self.year,
                month: 1,
                day: 1,
            }
            .days()
            + 1) as u16
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub month: u8,
    pub week: u8,
    pub weekday: u8,
    /// Seconds from local midnight, interpreted using the offset before the change.
    pub second: i32,
}
impl Rule {
    pub fn valid(self) -> bool {
        (1..=12).contains(&self.month)
            && (1..=5).contains(&self.week)
            && self.weekday < 7
            && (-604799..=604799).contains(&self.second)
    }
    pub fn instant(self, year: i32, before: i32) -> i64 {
        let first = Date {
            year,
            month: self.month,
            day: 1,
        }
        .days();
        let weekday = (first + 4).rem_euclid(7);
        let mut day =
            1 + (self.weekday as i64 - weekday).rem_euclid(7) + 7 * (self.week as i64 - 1);
        let next = if self.month == 12 {
            Date {
                year: year + 1,
                month: 1,
                day: 1,
            }
        } else {
            Date {
                year,
                month: self.month + 1,
                day: 1,
            }
        }
        .days();
        if first + day - 1 >= next {
            day -= 7;
        }
        (first + day - 1) * DAY + self.second as i64 - before as i64
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    pub standard: i32,
    pub daylight: i32,
    pub start: Rule,
    pub end: Rule,
}
impl Zone {
    pub const UTC: Self = Self {
        standard: 0,
        daylight: 0,
        start: Rule {
            month: 3,
            week: 2,
            weekday: 0,
            second: 7200,
        },
        end: Rule {
            month: 11,
            week: 1,
            weekday: 0,
            second: 7200,
        },
    };
    pub fn valid(self) -> bool {
        (-86400..=86400).contains(&self.standard)
            && (-86400..=86400).contains(&self.daylight)
            && self.start.valid()
            && self.end.valid()
            && (self.standard == self.daylight || self.start != self.end)
    }
    pub fn transitions(self, year: i32) -> [(i64, i32); 2] {
        [
            (self.start.instant(year, self.standard), self.daylight),
            (self.end.instant(year, self.daylight), self.standard),
        ]
    }
    pub fn offset(self, utc: i64) -> i32 {
        if self.standard == self.daylight {
            return self.standard;
        }
        let year = Date::at(utc).year;
        let mut best = (i64::MIN, self.standard);
        for y in year - 1..=year + 1 {
            for (at, offset) in self.transitions(y) {
                if at <= utc && at > best.0 {
                    best = (at, offset);
                }
            }
        }
        best.1
    }
    pub fn next_transition(self, utc: i64) -> Option<i64> {
        if self.standard == self.daylight {
            return None;
        }
        let year = Date::at(utc).year;
        let mut next = None;
        for y in year - 1..=year + 2 {
            for (at, _) in self.transitions(y) {
                if at > utc {
                    next = Some(next.map_or(at, |n: i64| n.min(at)));
                }
            }
        }
        next
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub latitude: i32,
    pub longitude: i32,
    pub zone: Zone,
}
impl Profile {
    pub fn valid(self) -> bool {
        (-90_000_000..=90_000_000).contains(&self.latitude)
            && (-180_000_000..=180_000_000).contains(&self.longitude)
            && self.zone.valid()
    }
    pub fn encode(self, out: &mut [u8; PROFILE_BYTES]) {
        out[..4].copy_from_slice(&self.latitude.to_le_bytes());
        out[4..8].copy_from_slice(&self.longitude.to_le_bytes());
        out[8..12].copy_from_slice(&self.zone.standard.to_le_bytes());
        out[12..16].copy_from_slice(&self.zone.daylight.to_le_bytes());
        for (r, b) in [self.zone.start, self.zone.end]
            .into_iter()
            .zip(out[16..].chunks_exact_mut(8))
        {
            b[..4].copy_from_slice(&[r.month, r.week, r.weekday, 0]);
            b[4..].copy_from_slice(&r.second.to_le_bytes());
        }
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != PROFILE_BYTES || b[19] != 0 || b[27] != 0 {
            return None;
        }
        let int = |at| i32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let rule = |at| Rule {
            month: b[at],
            week: b[at + 1],
            weekday: b[at + 2],
            second: int(at + 4),
        };
        let p = Self {
            latitude: int(0),
            longitude: int(4),
            zone: Zone {
                standard: int(8),
                daylight: int(12),
                start: rule(16),
                end: rule(24),
            },
        };
        p.valid().then_some(p)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Sunrise,
    Noon,
    Sunset,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boundary {
    pub event: Event,
    pub offset: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Predicate {
    Civil { start: u32, end: u32 },
    Solar { start: Boundary, end: Boundary },
}
impl Predicate {
    pub fn valid(self) -> bool {
        match self {
            Self::Civil { start, end } => start < 86400 && end <= 86400,
            Self::Solar { start, end } => {
                (-86400..=86400).contains(&start.offset) && (-86400..=86400).contains(&end.offset)
            }
        }
    }
    pub fn decode(b: &[u8]) -> Option<(Self, usize)> {
        let (p, n) = match *b.first()? {
            0 => (
                Self::Civil {
                    start: u32::from_le_bytes(b.get(1..5)?.try_into().ok()?),
                    end: u32::from_le_bytes(b.get(5..9)?.try_into().ok()?),
                },
                9,
            ),
            1 => {
                let boundary = |at| {
                    Some(Boundary {
                        event: match *b.get(at)? {
                            0 => Event::Sunrise,
                            1 => Event::Noon,
                            2 => Event::Sunset,
                            _ => return None,
                        },
                        offset: i32::from_le_bytes(b.get(at + 1..at + 5)?.try_into().ok()?),
                    })
                };
                (
                    Self::Solar {
                        start: boundary(1)?,
                        end: boundary(6)?,
                    },
                    11,
                )
            }
            _ => return None,
        };
        p.valid().then_some((p, n))
    }
    pub fn encode(self, out: &mut [u8; 11]) -> usize {
        match self {
            Self::Civil { start, end } => {
                out[0] = 0;
                out[1..5].copy_from_slice(&start.to_le_bytes());
                out[5..9].copy_from_slice(&end.to_le_bytes());
                9
            }
            Self::Solar { start, end } => {
                out[0] = 1;
                for (at, b) in [(1, start), (6, end)] {
                    out[at] = match b.event {
                        Event::Sunrise => 0,
                        Event::Noon => 1,
                        Event::Sunset => 2,
                    };
                    out[at + 1..at + 5].copy_from_slice(&b.offset.to_le_bytes());
                }
                11
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Profile,
    Configure {
        revision: u32,
        profile: Profile,
    },
    Read,
    Anchor {
        epoch: u64,
        revision: u32,
        utc_ms: i64,
    },
}
impl Request {
    pub fn encode(self, out: &mut [u8; 56]) -> usize {
        match self {
            Self::Profile => {
                out[0] = 15;
                1
            }
            Self::Configure { revision, profile } => {
                out[0] = 16;
                out[1..5].copy_from_slice(&revision.to_le_bytes());
                profile.encode((&mut out[5..37]).try_into().unwrap());
                37
            }
            Self::Read => {
                out[0] = 17;
                1
            }
            Self::Anchor {
                epoch,
                revision,
                utc_ms,
            } => {
                out[0] = 18;
                out[1..9].copy_from_slice(&epoch.to_le_bytes());
                out[9..13].copy_from_slice(&revision.to_le_bytes());
                out[13..21].copy_from_slice(&utc_ms.to_le_bytes());
                21
            }
        }
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        Some(match (*b.first()?, b.len()) {
            (15, 1) => Self::Profile,
            (16, 37) => Self::Configure {
                revision: u32::from_le_bytes(b[1..5].try_into().ok()?),
                profile: Profile::decode(&b[5..37])?,
            },
            (17, 1) => Self::Read,
            (18, 21) => Self::Anchor {
                epoch: u64::from_le_bytes(b[1..9].try_into().ok()?),
                revision: u32::from_le_bytes(b[9..13].try_into().ok()?),
                utc_ms: i64::from_le_bytes(b[13..21].try_into().ok()?),
            },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_and_transitions() {
        // One complete Gregorian leap cycle, including non-leap century 2100.
        let first = Date {
            year: 2000,
            month: 1,
            day: 1,
        }
        .days();
        for day in first..first + 146097 {
            assert_eq!(Date::from_days(day).days(), day);
        }
        let chicago = Zone {
            standard: -21600,
            daylight: -18000,
            ..Zone::UTC
        };
        let sydney = Zone {
            standard: 36000,
            daylight: 39600,
            start: Rule {
                month: 10,
                week: 1,
                weekday: 0,
                second: 7200,
            },
            end: Rule {
                month: 4,
                week: 1,
                weekday: 0,
                second: 10800,
            },
        };
        for (zone, date, second, before, after) in [
            (chicago, (2026, 3, 8), 8 * 3600, -21600, -18000),
            (chicago, (2026, 11, 1), 7 * 3600, -18000, -21600),
            (sydney, (2026, 4, 4), 16 * 3600, 39600, 36000),
            (sydney, (2026, 10, 3), 16 * 3600, 36000, 39600),
        ] {
            let at = Date {
                year: date.0,
                month: date.1,
                day: date.2,
            }
            .days()
                * DAY
                + second;
            assert_eq!(zone.offset(at - 1), before);
            assert_eq!(zone.offset(at), after);
            assert_eq!(zone.next_transition(at - 1), Some(at));
        }
    }
    #[test]
    fn numeric_encoding_bounds() {
        let p = Profile {
            latitude: 30_000_000,
            longitude: -97_000_000,
            zone: Zone::UTC,
        };
        let mut b = [0; PROFILE_BYTES];
        p.encode(&mut b);
        assert_eq!(Profile::decode(&b), Some(p));
        for (at, value) in [(19, 1), (27, 1), (16, 0), (17, 6), (18, 7)] {
            let mut bad = b;
            bad[at] = value;
            assert!(Profile::decode(&bad).is_none());
        }
        for request in [
            Request::Profile,
            Request::Read,
            Request::Configure {
                revision: 3,
                profile: p,
            },
            Request::Anchor {
                epoch: 1,
                revision: 3,
                utc_ms: MIN_UTC * 1000,
            },
        ] {
            let mut b = [0; 56];
            let n = request.encode(&mut b);
            assert_eq!(Request::decode(&b[..n]), Some(request));
            for end in 0..n {
                assert!(Request::decode(&b[..end]).is_none());
            }
            assert!(Request::decode(&b[..n + 1]).is_none());
        }
        for (start, end, valid) in [
            (0, 86400, true),
            (86399, 0, true),
            (86400, 0, false),
            (0, 86401, false),
        ] {
            assert_eq!(Predicate::Civil { start, end }.valid(), valid);
        }
    }
}
