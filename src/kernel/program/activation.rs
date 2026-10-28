//! Pending local causes, separate from the lossy outward observation ring.
//! A cursor advances only after its dependent activation has been admitted.

use core::sync::atomic::{AtomicU32, Ordering};
const COUNT: usize = crate::configLATTICE_ACTIVATIONS;
const _: () = assert!(COUNT > 0);
#[derive(Clone, Copy)]
pub(super) struct Change {
    pub cursor: u16,
}
struct Pending {
    records: [Option<Change>; COUNT],
    first: usize,
    count: usize,
}
static mut PENDING: Pending = Pending {
    records: [None; COUNT],
    first: 0,
    count: 0,
};
static OVERFLOW: AtomicU32 = AtomicU32::new(0);
fn access<R>(f: impl FnOnce(&mut Pending) -> R) -> R {
    struct Restore(crate::types::UBaseType_t);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::port::portCLEAR_INTERRUPT_MASK_FROM_ISR(self.0);
        }
    }
    let _restore = Restore(crate::port::portSET_INTERRUPT_MASK_FROM_ISR());
    f(unsafe { &mut *core::ptr::addr_of_mut!(PENDING) })
}
pub(crate) fn record(cursor: u16) {
    if cursor == u16::MAX {
        return;
    }
    access(|p| {
        if p.count == COUNT {
            OVERFLOW.fetch_add(1, Ordering::Relaxed);
            return;
        }
        p.records[(p.first + p.count) % COUNT] = Some(Change { cursor });
        p.count += 1;
    });
}
pub(super) fn first() -> Option<Change> {
    access(|p| p.records[p.first])
}
pub(super) fn advance(next: u16) {
    access(|p| {
        if next == u16::MAX {
            p.records[p.first] = None;
            p.first = (p.first + 1) % COUNT;
            p.count -= 1;
        } else {
            p.records[p.first].as_mut().unwrap().cursor = next;
        }
    });
}
/// Repair captured cursors before their storage can be reused. Other pending
/// causes keep their original order and continue through surviving siblings.
pub(super) fn redirect(from: u16, to: u16) {
    access(|p| {
        for change in p.records.iter_mut().flatten() {
            if change.cursor == from {
                change.cursor = to;
            }
        }
    });
}
pub(super) fn overflow() -> u32 {
    OVERFLOW.load(Ordering::Relaxed)
}
pub(super) fn pending() -> bool {
    access(|p| p.count != 0)
}

#[cfg(test)]
pub(crate) fn reset() {
    access(|p| {
        *p = Pending {
            records: [None; COUNT],
            first: 0,
            count: 0,
        }
    });
    OVERFLOW.store(0, Ordering::Relaxed);
}
#[cfg(all(test, feature = "port-test"))]
mod tests {
    use super::*;
    #[test]
    fn capacity_preserves_accepted_causes() {
        crate::port::test_port_reset();
        for n in [0, 1, COUNT, COUNT + 1, 2 * COUNT] {
            reset();
            for i in 0..n {
                record(i as u16 + 1);
            }
            assert_eq!(overflow(), n.saturating_sub(COUNT) as u32);
            for i in 0..n.min(COUNT) {
                assert_eq!(first().unwrap().cursor, i as u16 + 1);
                advance(20);
                assert_eq!(first().unwrap().cursor, 20);
                advance(u16::MAX);
            }
            assert!(!pending());
        }
        reset();
    }
}
