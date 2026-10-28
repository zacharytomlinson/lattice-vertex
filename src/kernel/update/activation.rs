//! Local update deferral. Recent activity is evidence, not a prediction of future
//! work. One small state machine shares the resident peer task's receive waits.
use super::package::Header;
use lattice_peer_wire::update::{Absence, AbsencePhase, PackageId, NOTICE_MS};

const MIN_QUIET_MS: u32 = 5_000;
const BACKOFF_MS: u32 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Wait,
    Announce(Absence),
    Cancel(Absence),
    Activate(Absence),
}

pub(crate) struct Activation {
    package: Option<PackageId>,
    quiet_since: u32,
    attempt: u32,
    announced: Option<u32>,
    backoff: u32,
}
impl Activation {
    pub const fn new() -> Self {
        Self {
            package: None,
            quiet_since: 0,
            attempt: 0,
            announced: None,
            backoff: 0,
        }
    }
    pub fn contending(&self) -> bool {
        self.announced.is_some()
    }
    pub fn activity(&mut self, now: u32) {
        self.quiet_since = now;
    }

    pub fn poll(
        &mut self,
        now: u32,
        local: u64,
        ready: Option<(Header, u32)>,
        busy: bool,
        neighbor_busy: bool,
    ) -> Action {
        let candidate = ready.and_then(|(h, swap)| h.costs.outage_ms(swap).map(|ms| (h.id, ms)));
        if self.package != candidate.map(|c| c.0) {
            let cancel = self.cancel();
            self.package = candidate.map(|c| c.0);
            self.quiet_since = now;
            self.backoff = jitter(local, now);
            if let Some(notice) = cancel {
                return Action::Cancel(notice);
            }
        }
        let Some((_, duration_ms)) = candidate else {
            return Action::Wait;
        };
        let quiet = duration_ms.max(MIN_QUIET_MS);
        if busy || neighbor_busy {
            self.quiet_since = now;
            self.backoff = jitter(local, now);
        }
        if now.wrapping_sub(self.quiet_since) < quiet.saturating_add(self.backoff) {
            return self.cancel().map_or(Action::Wait, Action::Cancel);
        }
        match self.announced {
            None => {
                self.attempt = if self.attempt == 0 {
                    now.max(1)
                } else {
                    self.attempt.wrapping_add(1).max(1)
                };
                Action::Announce(Absence {
                    attempt: self.attempt,
                    phase: AbsencePhase::Proposed,
                    duration_ms,
                })
            }
            Some(since) if now.wrapping_sub(since) >= NOTICE_MS => Action::Activate(Absence {
                attempt: self.attempt,
                phase: AbsencePhase::Leaving,
                duration_ms,
            }),
            Some(_) => Action::Wait,
        }
    }
    pub fn wait_ms(&self, now: u32, ready: Option<(Header, u32)>) -> Option<u32> {
        let (header, swap) = ready?;
        let duration = header.costs.outage_ms(swap)?;
        if self.package != Some(header.id) {
            return Some(0);
        }
        Some(if let Some(since) = self.announced {
            NOTICE_MS.saturating_sub(now.wrapping_sub(since))
        } else {
            duration
                .max(MIN_QUIET_MS)
                .saturating_add(self.backoff)
                .saturating_sub(now.wrapping_sub(self.quiet_since))
        })
    }
    pub fn announced(&mut self, now: u32, sent: bool) {
        if sent {
            self.announced = Some(now);
        } else {
            self.quiet_since = now;
        }
    }
    fn cancel(&mut self) -> Option<Absence> {
        self.announced.take().map(|_| Absence {
            attempt: self.attempt,
            phase: AbsencePhase::Cancel,
            duration_ms: 0,
        })
    }
}
fn jitter(local: u64, now: u32) -> u32 {
    let mut n = local as u32 ^ (local >> 32) as u32 ^ now;
    n ^= n << 13;
    n ^= n >> 17;
    n ^= n << 5;
    n % BACKOFF_MS
}

// Guards cover appliance work on other tasks as well as operations that outlive
// a native call. Commit closes admission atomically before changing boot state.
use core::sync::atomic::{AtomicU32, Ordering};
const COMMITTING: u32 = 1 << 31;
const OBSERVATION: u32 = 1 << 16;
const WORK_MASK: u32 = OBSERVATION - 1;
static HOLDS: AtomicU32 = AtomicU32::new(0);
static ACTIVITY: AtomicU32 = AtomicU32::new(0);

#[must_use = "keep the guard until the physical operation finishes"]
pub struct ActivationHold {
    count: u32,
}
impl Drop for ActivationHold {
    fn drop(&mut self) {
        if self.count == 1 {
            activity();
        }
        HOLDS.fetch_sub(self.count, Ordering::AcqRel);
        if self.count == 1 {
            wake();
        }
    }
}
/// Keep this guard for the duration of physical work that must not be interrupted
/// by automatic firmware activation. Busy means activation has already committed;
/// do not begin the operation. Ordinary `Appliance::with`/invocation holds itself.
pub fn hold_activation(
    _: &crate::sync::TaskContext,
) -> Result<ActivationHold, lattice_peer_wire::update::Status> {
    try_hold()
}
pub(crate) fn try_hold() -> Result<ActivationHold, lattice_peer_wire::update::Status> {
    acquire_hold(true)
}
pub(super) fn observation_hold() -> Result<ActivationHold, lattice_peer_wire::update::Status> {
    acquire_hold(false)
}
fn acquire_hold(
    activity_on_drop: bool,
) -> Result<ActivationHold, lattice_peer_wire::update::Status> {
    let count = if activity_on_drop { 1 } else { OBSERVATION };
    HOLDS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            if n >= COMMITTING || (activity_on_drop && n & WORK_MASK == WORK_MASK) {
                return None;
            }
            n.checked_add(count).filter(|next| *next < COMMITTING)
        })
        .map_err(|_| lattice_peer_wire::update::Status::Busy)?;
    if activity_on_drop {
        activity();
    }
    Ok(ActivationHold { count })
}
pub(crate) fn activity() {
    ACTIVITY.fetch_add(1, Ordering::AcqRel);
}
pub(crate) fn epoch() -> u32 {
    ACTIVITY.load(Ordering::Acquire)
}
/// Sampling blocks the atomic commit but does not restart the user-work quiet interval.
pub(crate) fn work_held() -> bool {
    HOLDS.load(Ordering::Acquire) & (WORK_MASK | COMMITTING) != 0
}
fn wake() {
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    crate::kernel::radio::task::notify_work();
}
pub(crate) struct Commit;
impl Commit {
    pub fn enter() -> Option<Self> {
        HOLDS
            .compare_exchange(0, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Commit {
    fn drop(&mut self) {
        HOLDS.store(0, Ordering::Release);
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::super::package::Costs;
    use super::*;
    fn header(known: bool) -> Header {
        Header {
            id: [1; 16],
            image_bytes: 4096,
            image_sha256: [0; 32],
            costs: Costs {
                erase_page_us: 100,
                write_per_byte: 256,
                verify_per_byte: 256,
                activation_us: if known { 1_000_000 } else { u32::MAX },
                swap_per_byte: 0,
                longest_pause_us: 100,
            },
        }
    }
    #[test]
    fn activity_holds_neighbors_and_unknown_costs_defer_with_wrap_safe_timing() {
        for origin in [0u32, u32::MAX - 3_000] {
            let at = |ms| origin.wrapping_add(ms);
            for (held, neighbor) in [(true, false), (false, true)] {
                let mut a = Activation::new();
                assert_eq!(
                    a.poll(at(0), 2, Some((header(false), 0)), false, false),
                    Action::Wait
                );
                assert_eq!(
                    a.poll(at(60_000), 2, Some((header(false), 0)), false, false),
                    Action::Wait
                );
                assert_eq!(
                    a.poll(at(0), 2, Some((header(true), 0)), false, false),
                    Action::Wait
                );
                assert_eq!(
                    a.poll(at(8_000), 2, Some((header(true), 0)), held, neighbor),
                    Action::Wait
                );
                let Action::Announce(n) =
                    a.poll(at(16_000), 2, Some((header(true), 0)), false, false)
                else {
                    panic!("not announced")
                };
                a.announced(at(16_000), true);
                a.activity(at(16_100));
                assert!(
                    matches!(a.poll(at(16_100), 2, Some((header(true), 0)), false, false), Action::Cancel(c) if c.attempt==n.attempt)
                );
                assert!(matches!(
                    a.poll(at(24_000), 2, Some((header(true), 0)), false, false),
                    Action::Announce(_)
                ));
                a.announced(at(24_000), true);
                assert_eq!(a.wait_ms(at(25_999), Some((header(true), 0))), Some(1));
                assert_eq!(a.wait_ms(at(26_000), Some((header(true), 0))), Some(0));
                assert_eq!(a.wait_ms(at(24_000), Some((header(false), 0))), None);
                assert_eq!(
                    a.poll(at(25_999), 2, Some((header(true), 0)), false, false),
                    Action::Wait
                );
                assert!(matches!(
                    a.poll(at(26_000), 2, Some((header(true), 0)), false, false),
                    Action::Activate(_)
                ));
            }
        }
        let mut a = Activation::new();
        let mut h = header(true);
        h.costs.activation_us = 25_000_000;
        a.poll(0, 1, Some((h, 380_928)), false, false);
        assert_eq!(
            a.poll(10_000, 1, Some((h, 380_928)), false, false),
            Action::Wait
        );
        assert!(matches!(
            a.poll(28_000, 1, Some((h, 380_928)), false, false),
            Action::Announce(_)
        ));
        a.announced(28_000, false);
        assert_eq!(
            a.poll(28_001, 1, Some((h, 380_928)), false, false),
            Action::Wait
        );
    }
    #[test]
    fn commit_and_physical_work_cannot_both_be_admitted() {
        let initial = epoch();
        let one = try_hold().unwrap();
        let two = try_hold().unwrap();
        assert!(work_held());
        assert!(Commit::enter().is_none());
        drop(one);
        assert!(Commit::enter().is_none());
        drop(two);
        assert!(!work_held());
        let commit = Commit::enter().unwrap();
        assert!(try_hold().is_err());
        drop(commit);
        assert!(try_hold().is_ok());
        assert_ne!(epoch(), initial);
    }
}
