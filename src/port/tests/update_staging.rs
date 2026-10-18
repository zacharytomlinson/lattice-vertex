//! Maintained background-staging check: emulate critical appliance work once a
//! package is accepted. The public hold leaves normal host/peer service running.
use lattice_vertex::{
    kernel::{tasks, update},
    sync::TaskContext,
};

pub struct Check {
    hold: Option<update::UpdateHold>,
    held_at: Option<u32>,
    completed: bool,
}
impl Check {
    pub const fn new() -> Self {
        Self {
            hold: None,
            held_at: None,
            completed: false,
        }
    }
    pub fn poll(&mut self, context: &TaskContext) {
        if self.completed {
            return;
        }
        let now = tasks::xTaskGetTickCount();
        if let Some(since) = self.held_at {
            if now.wrapping_sub(since)
                >= lattice_vertex::milliseconds_to_ticks_checked(10_000).unwrap()
            {
                drop(self.hold.take());
                self.completed = true;
            }
        } else if update::snapshot(context).progress.phase == update::Phase::Preparing {
            if let Ok(hold) = update::hold_updates(context) {
                self.hold = Some(hold);
                self.held_at = Some(now);
            }
        }
    }
}
