//! Confirmation requires local execution progress, not a network quorum.
use lattice_peer_wire::update::Status;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Waiting,
    Confirmed,
    Failed,
}
pub(crate) struct Startup {
    started: u32,
    radio_completed: bool,
    state: State,
}
impl Startup {
    pub const fn new(started: u32) -> Self {
        Self {
            started,
            radio_completed: false,
            state: State::Waiting,
        }
    }
    pub fn confirmed(&self) -> bool {
        self.state == State::Confirmed
    }
    /// A failed persistent confirmation is not retried. Stop feeding and let the
    /// loader resolve boot metadata on the hardware reset.
    pub fn progress(
        &mut self,
        now: u32,
        radio_completed: bool,
        confirm: impl FnOnce() -> Result<(), Status>,
    ) -> bool {
        self.radio_completed |= radio_completed;
        if self.state == State::Waiting
            && self.radio_completed
            && now.wrapping_sub(self.started) != 0
        {
            self.state = if confirm().is_ok() {
                State::Confirmed
            } else {
                State::Failed
            };
        }
        self.confirmed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_requires_radio_and_tick_progress_and_is_performed_once() {
        for started in [0, u32::MAX] {
            for radio_first in [false, true] {
                let mut state = Startup::new(started);
                assert!(!state.progress(started, radio_first, || panic!("no tick progress")));
                if !radio_first {
                    assert!(!state.progress(started.wrapping_add(1), false, || panic!(
                        "no radio progress"
                    )));
                }
                let mut writes = 0;
                assert!(state.progress(started.wrapping_add(2), true, || {
                    writes += 1;
                    Ok(())
                }));
                assert!(
                    state.progress(started.wrapping_add(3), false, || panic!("confirmed twice"))
                );
                assert_eq!(writes, 1);
            }
        }
    }
    #[test]
    fn uncertain_confirmation_stops_watchdog_feeding_without_another_flash_write() {
        let mut state = Startup::new(0);
        assert!(!state.progress(1, true, || Err(Status::Storage)));
        assert!(!state.progress(2, true, || panic!("must reset after an uncertain write")));
        assert_eq!(state.state, State::Failed);
    }
}
