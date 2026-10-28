//! Flash admission and measured pacing on the existing peer task. No queued
//! operation is retained: a deferred immutable request returns Busy to its sender.
use super::{activation, package::Costs, Status};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// Enough space between flash steps for a maximum 802.15.4 frame/turnaround.
const RECEIVE_TURN_MS: u32 = crate::kernel::radio::receive::receive_turn_ticks(1000);
const VERIFY_TARGET_US: u64 = 1_000;
const MAX_VERIFY_BYTES: u32 = 1024;

pub(super) fn verification_bytes(costs: Costs) -> u32 {
    match costs.verify_per_byte {
        u32::MAX => 256, // unknown estimate: one fixed scratch-buffer read
        0 => MAX_VERIFY_BYTES,
        cost => (VERIFY_TARGET_US * 256 / u64::from(cost)).clamp(1, MAX_VERIFY_BYTES as u64) as u32,
    }
}

#[derive(Clone, Copy)]
pub(super) enum Work {
    ErasePage,
    Write(u32),
    Verify(u32),
}
impl Work {
    fn estimate_us(self, costs: Costs) -> Option<u64> {
        let (coefficient, quantity, divisor) = match self {
            Self::ErasePage => (costs.erase_page_us, 1, 1),
            Self::Write(bytes) => (costs.write_per_byte, bytes, 256),
            Self::Verify(bytes) => (costs.verify_per_byte, bytes, 256),
        };
        (coefficient != u32::MAX)
            .then(|| (u64::from(coefficient) * u64::from(quantity)).div_ceil(divisor))
    }
}

pub(super) struct Staging {
    activity_epoch: u32,
    quiet_since: u32,
    completed_at: u32,
    service_ms: u32,
}
impl Staging {
    pub const fn new() -> Self {
        Self {
            activity_epoch: 0,
            quiet_since: 0,
            completed_at: 0,
            service_ms: 0,
        }
    }
    fn interval(work: Work, costs: Costs) -> u32 {
        work.estimate_us(costs).map_or(RECEIVE_TURN_MS, |us| {
            u32::try_from(us.div_ceil(1000))
                .unwrap_or(u32::MAX)
                .max(RECEIVE_TURN_MS)
        })
    }
    pub fn admit(
        &mut self,
        now: u32,
        activity_epoch: u32,
        busy: bool,
        work: Work,
        costs: Costs,
    ) -> bool {
        if busy || activity_epoch != self.activity_epoch {
            self.activity_epoch = activity_epoch;
            self.quiet_since = now;
        }
        !busy
            && now.wrapping_sub(self.quiet_since) >= Self::interval(work, costs)
            && now.wrapping_sub(self.completed_at) >= self.service_ms
    }
    pub fn completed(&mut self, now: u32, work: Work, costs: Costs) {
        self.completed_at = now;
        // Reserve a normal-service interval after synchronous work, even on
        // ports where flash stalls caused elapsed RTOS ticks to be lost.
        self.service_ms = Self::interval(work, costs);
    }
}

// The high bit owns flash; the remaining bits count appliance access. Separate
// from activation holds: an operation may tolerate staging but prohibit reboot.
const FLASH: u32 = 1 << 31;
static ACCESS: AtomicU32 = AtomicU32::new(0);
static FLASH_WAITING: AtomicBool = AtomicBool::new(false);
struct Access;
impl Drop for Access {
    fn drop(&mut self) {
        if ACCESS.fetch_sub(1, Ordering::AcqRel) == 1 && FLASH_WAITING.swap(false, Ordering::AcqRel)
        {
            #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
            crate::kernel::radio::task::notify_work();
        }
    }
}

/// Keep for physical work that cannot tolerate flash stalls or automatic reboot.
/// Busy means a flash step or activation already started; do not begin that work.
#[must_use = "retain the guard until the critical physical work finishes"]
pub struct UpdateHold {
    // Release staging before the activation guard publishes activity and wakes
    // the peer, so a resumed peer sees the completed appliance access.
    _access: Access,
    _activation: super::ActivationHold,
}
pub fn hold_updates(_: &crate::sync::TaskContext) -> Result<UpdateHold, Status> {
    hold_access(activation::try_hold()?)
}
pub(crate) fn hold_observation(_: &crate::sync::TaskContext) -> Result<UpdateHold, Status> {
    hold_access(activation::observation_hold()?)
}
fn hold_access(hold: super::ActivationHold) -> Result<UpdateHold, Status> {
    ACCESS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < FLASH - 1).then_some(n + 1)
        })
        .map_err(|_| Status::Busy)?;
    Ok(UpdateHold {
        _access: Access,
        _activation: hold,
    })
}

pub(crate) struct FlashStep {
    _private: (),
}
impl FlashStep {
    pub fn enter() -> Option<Self> {
        ACCESS
            .compare_exchange(0, FLASH, Ordering::AcqRel, Ordering::Acquire)
            .or_else(|_| {
                FLASH_WAITING.store(true, Ordering::Release);
                ACCESS.compare_exchange(0, FLASH, Ordering::AcqRel, Ordering::Acquire)
            })
            .ok()
            .map(|_| Self { _private: () })
    }
}
impl Drop for FlashStep {
    fn drop(&mut self) {
        ACCESS.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn costs() -> Costs {
        Costs {
            erase_page_us: 80_000,
            write_per_byte: 256,
            verify_per_byte: 512,
            activation_us: 0,
            swap_per_byte: 0,
            longest_pause_us: 80_000,
        }
    }
    #[test]
    fn measured_work_yields_normal_service_and_restarts_quiet_after_activity() {
        for origin in [0u32, u32::MAX - 50] {
            let at = |ms| origin.wrapping_add(ms);
            let mut p = Staging::new();
            assert!(!p.admit(at(0), 1, false, Work::ErasePage, costs()));
            assert!(!p.admit(at(79), 1, false, Work::ErasePage, costs()));
            assert!(p.admit(at(80), 1, false, Work::ErasePage, costs()));
            p.completed(at(100), Work::ErasePage, costs());
            assert!(!p.admit(at(179), 1, false, Work::Write(48), costs()));
            assert!(p.admit(at(180), 1, false, Work::Write(48), costs()));
            assert!(!p.admit(at(181), 1, true, Work::Write(48), costs()));
            assert!(!p.admit(at(182), 2, false, Work::Write(48), costs()));
            assert!(!p.admit(at(187), 2, false, Work::Write(48), costs()));
            assert!(p.admit(at(188), 2, false, Work::Write(48), costs()));
            let unknown = Costs {
                erase_page_us: u32::MAX,
                ..costs()
            };
            assert_eq!(Work::ErasePage.estimate_us(unknown), None);
            assert_eq!(Staging::interval(Work::ErasePage, unknown), RECEIVE_TURN_MS);
        }
    }
    #[test]
    fn verification_quantity_follows_per_byte_cost_without_unbounded_work() {
        for (cost, bytes) in [
            (0, 1024),
            (256, 1000),
            (512, 500),
            (u32::MAX - 1, 1),
            (u32::MAX, 256),
        ] {
            assert_eq!(
                verification_bytes(Costs {
                    verify_per_byte: cost,
                    ..costs()
                }),
                bytes
            );
        }
    }
    #[test]
    fn reboot_holds_allow_staging_but_appliance_access_excludes_flash() {
        let context = unsafe { crate::sync::TaskContext::assume() };
        let epoch = activation::epoch();
        let observation = hold_observation(&context).unwrap();
        assert!(FlashStep::enter().is_none());
        assert!(FLASH_WAITING.load(Ordering::Acquire));
        assert!(activation::Commit::enter().is_none());
        assert_eq!(activation::epoch(), epoch);
        assert!(
            !activation::work_held(),
            "sampling must not restart the quiet interval"
        );
        drop(observation);
        assert!(!FLASH_WAITING.load(Ordering::Acquire));
        assert_eq!(
            activation::epoch(),
            epoch,
            "sampling alone is not user activity"
        );
        let reboot = activation::try_hold().unwrap();
        let flash = FlashStep::enter().unwrap();
        assert!(hold_updates(&context).is_err());
        assert!(activation::Commit::enter().is_none());
        drop(flash);
        let physical = hold_updates(&context).unwrap();
        assert!(FlashStep::enter().is_none());
        drop(physical);
        assert!(FlashStep::enter().is_some());
        assert!(
            activation::work_held(),
            "dropping access must preserve the independent reboot hold"
        );
        drop(reboot);
        assert!(!activation::work_held());
    }
}
