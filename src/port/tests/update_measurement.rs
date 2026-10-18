//! Local update timing readiness and stack checks through existing kernel hooks.
use core::sync::atomic::{AtomicBool, Ordering};
static PEER_READY: AtomicBool = AtomicBool::new(false);
pub fn start() {
    lattice_vertex::kernel::radio::testing::observe(|event| {
        if matches!(
            event,
            lattice_vertex::kernel::radio::testing::Event::NetworkSelected { .. }
        ) {
            PEER_READY.store(true, Ordering::Release);
        }
    });
}
pub fn peer_ready() -> bool {
    PEER_READY.load(Ordering::Acquire)
}
pub fn radio_free_bytes() -> usize {
    use lattice_vertex::{
        kernel::tasks,
        types::{StackType_t, TaskStatus_t},
    };
    let mut status = [const { TaskStatus_t::new() }; 4];
    // Fixtures retain all task storage until reset; this scan runs in task context.
    let count = unsafe {
        tasks::uxTaskGetSystemState(
            status.as_mut_ptr(),
            status.len() as _,
            core::ptr::null_mut(),
        )
    };
    for task in &status[..count as usize] {
        if !task.pcTaskName.is_null()
            && unsafe { core::slice::from_raw_parts(task.pcTaskName, 6) } == b"radio\0"
        {
            return task.usStackHighWaterMark as usize * core::mem::size_of::<StackType_t>();
        }
    }
    0
}
