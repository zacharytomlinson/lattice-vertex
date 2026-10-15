//! Registered observation deltas awaiting the resident radio task.
//! This is a fixed transport ring, not a graph work queue or state mirror.

use super::appliance::Native;
use crate::{ir::Value, port, types::UBaseType_t};

const CAPACITY: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub attribute: u64,
    pub sequence: u32,
    pub value: Option<Value>,
}

struct Outgoing {
    native: Option<&'static dyn Native>,
    records: [Option<Change>; CAPACITY],
    first: usize,
    count: usize,
    sequence: u32,
    evicted: u32,
}
impl Outgoing {
    const fn new() -> Self {
        Self {
            native: None,
            records: [None; CAPACITY],
            first: 0,
            count: 0,
            sequence: 0,
            evicted: 0,
        }
    }
}
static mut OUTGOING: Outgoing = Outgoing::new();

fn access<R>(f: impl FnOnce(&mut Outgoing) -> R) -> R {
    struct Restore(UBaseType_t);
    impl Drop for Restore {
        fn drop(&mut self) {
            port::portCLEAR_INTERRUPT_MASK_FROM_ISR(self.0);
        }
    }
    let _restore = Restore(port::portSET_INTERRUPT_MASK_FROM_ISR());
    // Supported ports are single-core; the preserved interrupt mask protects
    // both producer and radio-task access. No hardware/native operation runs here.
    f(unsafe { &mut *core::ptr::addr_of_mut!(OUTGOING) })
}

pub(crate) fn bind(native: &'static dyn Native) {
    assert!(native.ready(), "initialize appliance before kernel startup");
    access(|out| {
        assert!(out.native.is_none(), "appliance already bound");
        out.native = Some(native);
    });
}

pub(crate) fn record(instance: *const (), attribute: u64, value: Option<Value>) {
    let recorded = access(|out| {
        if !out.native.is_some_and(|n| n.owns(instance, attribute)) {
            return false;
        }
        out.sequence = out.sequence.wrapping_add(1);
        if out.count == CAPACITY {
            out.records[out.first] = None;
            out.first = (out.first + 1) % CAPACITY;
            out.count -= 1;
            out.evicted = out.evicted.wrapping_add(1);
        }
        out.records[(out.first + out.count) % CAPACITY] = Some(Change {
            attribute,
            sequence: out.sequence,
            value,
        });
        out.count += 1;
        true
    });
    if recorded {
        #[cfg(any(feature = "port-esp32c6", feature = "port-nrf52840"))]
        super::radio::task::notify_work();
    }
}

pub(crate) fn pending() -> bool {
    access(|out| out.count != 0)
}

/// Latest registered local change, including records already sent or evicted.
/// Sample with the appliance mutex held when pairing this watermark with state.
pub(crate) fn sequence() -> u32 {
    access(|out| out.sequence)
}

pub(crate) fn take() -> Option<Change> {
    access(|out| {
        if out.count == 0 {
            return None;
        }
        let change = out.records[out.first].take();
        out.first = (out.first + 1) % CAPACITY;
        out.count -= 1;
        change
    })
}
pub(crate) fn evicted() -> u32 {
    access(|out| out.evicted)
}

#[cfg(test)]
pub(crate) fn reset() {
    access(|out| *out = Outgoing::new());
}
