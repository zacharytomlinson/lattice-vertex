//! Local commissioned time and predicate deadlines. One resident peer task owns
//! this state; RTOS delayed wakeups service the earliest boundary, not a clock scan.
mod solar;
use lattice_peer_wire::clock::{Date, Event, Predicate, Profile, DAY, MAX_UTC, MIN_UTC};
pub(crate) const CAPACITY: usize = 32;
pub(crate) const SOURCE: u16 = 0x8000;
#[derive(Clone, Copy)]
struct Alarm {
    source: u16,
    predicate: Predicate,
    value: Option<bool>,
    next: Option<i64>,
}
pub(crate) struct Time {
    pub profile: Option<Profile>,
    anchor: Option<(i64, u64)>,
    revision: u32,
    caller: u64,
    last: i64,
    alarms: [Option<Alarm>; CAPACITY],
    count: usize,
    next: Option<i64>,
}
impl Time {
    pub const fn new() -> Self {
        Self {
            profile: None,
            anchor: None,
            revision: 0,
            caller: 0,
            last: 0,
            alarms: [None; CAPACITY],
            count: 0,
            next: None,
        }
    }
    pub fn utc(&self, mono: u64) -> Option<i64> {
        let (utc, at) = self.anchor?;
        let elapsed = i64::try_from(mono.checked_sub(at)?).ok()?;
        utc.checked_add(elapsed).filter(|t| valid_utc(*t))
    }
    pub fn revision(&self) -> u32 {
        self.revision
    }
    pub fn anchor(
        &mut self,
        caller: u64,
        revision: u32,
        utc: i64,
        mono: u64,
    ) -> Result<(), lattice_peer_wire::program::transfer::Status> {
        use lattice_peer_wire::program::transfer::Status;
        if caller == 0 || caller == u64::MAX {
            return Err(Status::Invalid);
        }
        if !valid_utc(utc) {
            return Err(Status::Bounds);
        }
        if revision.checked_add(1) == Some(self.revision)
            && caller == self.caller
            && utc == self.last
        {
            return Ok(());
        }
        if revision != self.revision {
            return Err(Status::Conflict);
        }
        self.revision = self.revision.checked_add(1).ok_or(Status::Bounds)?;
        self.anchor = Some((utc, mono));
        self.caller = caller;
        self.last = utc;
        self.refresh(mono, false);
        Ok(())
    }
    pub fn configure(&mut self, profile: Option<Profile>, mono: u64) {
        if self.profile != profile {
            self.profile = profile;
            self.refresh(mono, false);
        }
    }
    pub fn add(&mut self, source: u16, predicate: Predicate, mono: u64) -> Result<(), ()> {
        if self.alarms[..self.count]
            .iter()
            .flatten()
            .any(|a| a.source == source)
        {
            return Ok(());
        }
        if self.count == CAPACITY {
            return Err(());
        }
        let (value, next) = self.sample(predicate, mono);
        self.alarms[self.count] = Some(Alarm {
            source,
            predicate,
            value,
            next,
        });
        self.count += 1;
        self.rearm();
        Ok(())
    }
    pub fn retain(&mut self, mut keep: impl FnMut(u16) -> bool) {
        let mut count = 0;
        for i in 0..self.count {
            let alarm = self.alarms[i].unwrap();
            if keep(alarm.source) {
                self.alarms[count] = Some(alarm);
                count += 1;
            }
        }
        self.alarms[count..].fill(None);
        self.count = count;
        self.rearm();
    }
    pub fn sample(&self, predicate: Predicate, mono: u64) -> (Option<bool>, Option<i64>) {
        let (Some(profile), Some(utc)) = (self.profile, self.utc(mono)) else {
            return (None, None);
        };
        let (value, next) = evaluate(profile, predicate, utc.div_euclid(1000));
        (value, next.and_then(|t| t.checked_mul(1000)))
    }
    fn refresh(&mut self, mono: u64, only_due: bool) {
        let utc = self.utc(mono);
        for i in 0..self.count {
            let old = self.alarms[i].unwrap();
            if only_due && !old.next.is_some_and(|n| utc.is_none_or(|u| u >= n)) {
                continue;
            }
            let (value, next) = self.sample(old.predicate, mono);
            // The first valid reference establishes a baseline. Enable/recovery
            // visits already wait for it; subsequent truth changes activate.
            if old.value.is_some() && old.value != value {
                crate::kernel::program::activation::record(old.source | SOURCE);
            }
            self.alarms[i] = Some(Alarm { value, next, ..old });
        }
        self.rearm();
    }
    pub fn poll(&mut self, mono: u64) {
        if self.wait_ms(mono) == Some(0) {
            self.refresh(mono, true);
        }
    }
    pub fn wait_ms(&self, mono: u64) -> Option<u64> {
        let next = self.next?;
        Some(
            self.utc(mono)
                .map_or(0, |utc| next.saturating_sub(utc).max(0) as u64),
        )
    }
    fn rearm(&mut self) {
        self.next = self.alarms[..self.count]
            .iter()
            .flatten()
            .filter_map(|a| a.next)
            .min();
    }
    pub fn value(&self, source: u16) -> Option<bool> {
        self.alarms[..self.count]
            .iter()
            .flatten()
            .find(|a| a.source == source)?
            .value
    }
}
pub(crate) fn valid_utc(ms: i64) -> bool {
    (MIN_UTC * 1000..MAX_UTC * 1000).contains(&ms)
}
pub(crate) fn monotonic_ms() -> u64 {
    #[cfg(feature = "port-esp32c6")]
    return esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis();
    #[cfg(all(feature = "port-nrf52840", feature = "nrf52840-update"))]
    return cortex_m::interrupt::free(|_| {
        ticks_ms(extended_ticks() + crate::port::nrf52840::update::elapsed_ticks())
    });
    #[cfg(not(any(
        feature = "port-esp32c6",
        all(feature = "port-nrf52840", feature = "nrf52840-update")
    )))]
    ticks_ms(extended_ticks())
}
fn extended_ticks() -> u64 {
    let mut time = crate::kernel::tasks::TimeOut_t::new();
    unsafe {
        crate::kernel::tasks::vTaskSetTimeOutState(&mut time);
    }
    if crate::types::TickType_t::BITS == 32 {
        ((time.xOverflowCount as u32 as u64) << 32) | (time.xTimeOnEntering as u64)
    } else {
        time.xTimeOnEntering as u64
    }
}
fn ticks_ms(ticks: u64) -> u64 {
    ((ticks as u128 * 1000) / crate::configTICK_RATE_HZ as u128).min(u64::MAX as u128) as u64
}
fn minimum(next: &mut Option<i64>, candidate: i64, now: i64) {
    if candidate > now {
        *next = Some(next.map_or(candidate, |n| n.min(candidate)));
    }
}

pub(crate) fn evaluate(
    profile: Profile,
    predicate: Predicate,
    utc: i64,
) -> (Option<bool>, Option<i64>) {
    match predicate {
        Predicate::Civil { start, end } => {
            let zone = profile.zone;
            let local = utc + zone.offset(utc) as i64;
            let second = local.rem_euclid(DAY) as u32;
            let value = if start <= end {
                second >= start && second < end
            } else {
                second >= start || second < end
            };
            if start == end || (start == 0 && end == 86400) {
                return (Some(value), None);
            }
            let mut next = zone.next_transition(utc);
            let day = local.div_euclid(DAY);
            for day in day - 1..=day + 2 {
                for boundary in [start, end] {
                    for offset in [zone.standard, zone.daylight] {
                        let candidate = day * DAY + boundary as i64 - offset as i64;
                        if zone.offset(candidate) == offset {
                            minimum(&mut next, candidate, utc);
                        }
                    }
                }
            }
            (Some(value), next)
        }
        Predicate::Solar { start, end } => {
            if start == end {
                return (Some(false), None);
            }
            let today = Date::at(utc).days();
            let mut previous = [None, None];
            let mut next = None;
            for day in today - 2..=today + 2 {
                for (i, b) in [start, end].into_iter().enumerate() {
                    if let solar::Crossing::At(at) = solar::event(
                        Date::from_days(day),
                        profile.latitude,
                        profile.longitude,
                        b.event,
                    ) {
                        let at = at + b.offset as i64;
                        if at <= utc {
                            previous[i] = Some(previous[i].map_or(at, |p: i64| p.max(at)));
                        }
                        minimum(&mut next, at, utc);
                    }
                }
            }
            // A missing polar crossing is unavailable, not midnight. Recompute
            // on the next day so an eventual seasonal crossing becomes known.
            minimum(&mut next, (today + 1) * DAY, utc);
            let mut value = previous[0].zip(previous[1]).map(|(a, b)| a > b);
            if start.offset == 0
                && end.offset == 0
                && matches!(
                    (start.event, end.event),
                    (Event::Sunrise, Event::Sunset) | (Event::Sunset, Event::Sunrise)
                )
            {
                value = match solar::event(
                    Date::from_days(today),
                    profile.latitude,
                    profile.longitude,
                    Event::Sunrise,
                ) {
                    solar::Crossing::Above => Some(start.event == Event::Sunrise),
                    solar::Crossing::Below => Some(start.event == Event::Sunset),
                    _ => value,
                };
            }
            (value, next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_peer_wire::clock::{Boundary, Zone};
    const PROFILE: Profile = Profile {
        latitude: 30_267_200,
        longitude: -97_743_100,
        zone: Zone {
            standard: -21600,
            daylight: -18000,
            ..Zone::UTC
        },
    };
    fn at(month: u8, day: u8, second: i64) -> i64 {
        Date {
            year: 2026,
            month,
            day,
        }
        .days()
            * DAY
            + second
    }
    #[test]
    fn civil_boundaries() {
        for (month, day, utc, start, end, value, delta) in [
            (1, 1, 19 * 3600, 14 * 3600, 16 * 3600, false, 3600),
            (1, 1, 20 * 3600, 14 * 3600, 16 * 3600, true, 7200),
            (1, 1, 22 * 3600, 14 * 3600, 16 * 3600, false, 79200),
            (1, 1, 5 * 3600, 22 * 3600, 2 * 3600, true, 10800),
            // Spring gap: 02:30 never occurs; entry happens at the DST jump.
            (3, 8, 8 * 3600 - 1, 9000, 14400, false, 1),
            (3, 8, 8 * 3600, 9000, 14400, true, 3600),
            // Autumn fold: 01:30-02:00 becomes false at rollback then true again.
            (11, 1, 7 * 3600 - 1, 5400, 7200, true, 1),
            (11, 1, 7 * 3600, 5400, 7200, false, 1800),
        ] {
            let now = at(month, day, utc);
            assert_eq!(
                evaluate(PROFILE, Predicate::Civil { start, end }, now),
                (Some(value), Some(now + delta))
            );
        }
    }
    #[test]
    fn solar_bounds() {
        use solar::Crossing::*;
        for (lat, lon) in [
            (0, 0),
            (30_267_200, -97_743_100),
            (-33_868_800, 151_209_300),
            (0, 179_000_000),
            (0, -179_000_000),
        ] {
            for (month, day) in [(3, 20), (6, 21), (12, 21)] {
                let d = Date {
                    year: 2026,
                    month,
                    day,
                };
                let [At(rise), At(noon), At(set)] = [Event::Sunrise, Event::Noon, Event::Sunset]
                    .map(|e| solar::event(d, lat, lon, e))
                else {
                    panic!("non-polar crossing missing")
                };
                assert!(rise < noon && noon < set && set - rise < DAY);
                assert!(
                    (noon + lon as i64 * DAY / 360_000_000 - d.days() * DAY - DAY / 2).abs() < 1200
                );
            }
        }
        let daylight = Predicate::Solar {
            start: Boundary {
                event: Event::Sunrise,
                offset: 0,
            },
            end: Boundary {
                event: Event::Sunset,
                offset: 0,
            },
        };
        for (lat, month, truth) in [
            (89_000_000, 6, true),
            (89_000_000, 12, false),
            (-89_000_000, 6, false),
            (-89_000_000, 12, true),
        ] {
            assert_eq!(
                evaluate(
                    Profile {
                        latitude: lat,
                        ..PROFILE
                    },
                    daylight,
                    at(month, 21, 0)
                )
                .0,
                Some(truth)
            );
        }
    }
    #[test]
    fn anchoring_and_alarm_capacity() {
        use lattice_peer_wire::program::transfer::Status;
        let mut t = Time::new();
        assert_eq!(t.utc(0), None);
        t.configure(Some(PROFILE), 0);
        let utc = at(1, 1, 20 * 3600) * 1000;
        assert_eq!(t.anchor(1, 0, utc, 100), Ok(()));
        assert_eq!(t.anchor(1, 0, utc, 500), Ok(())); // duplicate cannot rewind
        assert_eq!(t.utc(500), Some(utc + 400));
        assert_eq!(t.anchor(2, 0, utc, 500), Err(Status::Conflict));
        for n in 0..CAPACITY {
            t.add(
                n as u16,
                Predicate::Civil {
                    start: 14 * 3600,
                    end: 16 * 3600,
                },
                100,
            )
            .unwrap();
        }
        assert_eq!(
            t.add(CAPACITY as u16, Predicate::Civil { start: 0, end: 1 }, 100),
            Err(())
        );
        assert_eq!(t.wait_ms(100), Some(7_200_000));
        assert_eq!(t.value(0), Some(true));
        t.retain(|source| source % 2 == 0);
        assert_eq!(t.count, CAPACITY / 2);
        for source in 0..CAPACITY as u16 {
            assert_eq!(t.value(source).is_some(), source % 2 == 0);
        }
        t.retain(|_| false);
        assert_eq!(t.wait_ms(100), None);
        assert!(t.add(0, Predicate::Civil { start: 0, end: 1 }, 100).is_ok());
        assert_eq!(Time::new().utc(100), None); // persisted profile is not an outage clock
        for (ms, valid) in [
            (MIN_UTC * 1000 - 1, false),
            (MIN_UTC * 1000, true),
            (MAX_UTC * 1000 - 1, true),
            (MAX_UTC * 1000, false),
        ] {
            assert_eq!(valid_utc(ms), valid);
        }
    }
}
