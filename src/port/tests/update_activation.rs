//! Maintained check: emulate physical work for ten seconds after verification.
//! Uses the same public hold as an appliance; no test control enters the kernel.
use lattice_vertex::{
    kernel::{tasks, update},
    sync::TaskContext,
};

pub struct Check {
    hold: Option<update::ActivationHold>,
    ready_at: Option<u32>,
}
impl Check {
    pub fn new(context: &TaskContext) -> Self {
        Self {
            hold: Some(update::hold_activation(context).unwrap()),
            ready_at: None,
        }
    }
    pub fn poll(&mut self, context: &TaskContext) {
        if self.hold.is_none() {
            return;
        }
        if update::snapshot(context).progress.phase != update::Phase::Ready {
            return;
        }
        let now = tasks::xTaskGetTickCount();
        let since = *self.ready_at.get_or_insert(now);
        if now.wrapping_sub(since) >= lattice_vertex::milliseconds_to_ticks_checked(10_000).unwrap()
        {
            drop(self.hold.take());
        }
    }
}
