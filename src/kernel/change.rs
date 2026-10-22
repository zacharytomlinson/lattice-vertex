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
    batches: u32,
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
            batches: 0,
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
    native.seal();
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
        if let Some((_, head)) = out.native.and_then(|n| n.state_binding(attribute)) {
            super::program::activation::record(head.load(core::sync::atomic::Ordering::Acquire));
        }
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
        #[cfg(feature = "firmware-update")]
        super::update::activation::activity();
        #[cfg(any(feature = "port-esp32c6", feature = "port-nrf52840"))]
        if pending() {
            super::radio::task::notify_work();
        }
    }
}

/// Hold transport publication until native samples and derived state are committed.
/// This does not merge records or change their hard-eviction policy.
pub(crate) struct Batch;
pub(crate) fn batch() -> Batch {
    access(|out| out.batches = out.batches.checked_add(1).expect("observation batch depth"));
    Batch
}
impl Drop for Batch {
    fn drop(&mut self) {
        access(|out| {
            assert!(out.batches != 0);
            out.batches -= 1;
        });
        #[cfg(any(feature = "port-esp32c6", feature = "port-nrf52840"))]
        if pending() {
            super::radio::task::notify_work();
        }
    }
}

pub(crate) fn pending() -> bool {
    access(|out| out.count != 0 && out.batches == 0)
}
pub(crate) fn committed() -> bool {
    access(|out| out.batches == 0)
}

/// Latest registered local change, including records already sent or evicted.
/// Sample with the observation-cache lock when pairing this watermark with state.
pub(crate) fn sequence() -> u32 {
    access(|out| out.sequence)
}

pub(crate) fn take() -> Option<Change> {
    access(|out| {
        if out.count == 0 || out.batches != 0 {
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
    super::program::activation::reset();
}

#[cfg(all(test, feature = "port-test"))]
mod tests {
    use super::*;
    use crate::{
        kernel::peer::frame::{InvokeStatus, StateReadResult},
        sync::TaskContext,
    };
    static INSTANCE: u8 = 0;
    struct Owner;
    impl Native for Owner {
        fn ready(&self) -> bool {
            true
        }
        fn owns(&self, instance: *const (), attribute: u64) -> bool {
            instance == core::ptr::from_ref(&INSTANCE).cast() && attribute == 1
        }
        fn read(&self, _: &TaskContext, _: u64) -> StateReadResult {
            unreachable!()
        }
        fn invoke(&self, _: &TaskContext, _: u64, _: &[Value]) -> InvokeStatus {
            unreachable!()
        }
    }
    #[test]
    fn transport_bounds() {
        crate::port::test_port_reset();
        for count in [0, 1, CAPACITY - 1, CAPACITY, CAPACITY + 1, 2 * CAPACITY + 1] {
            reset();
            bind(&Owner);
            for i in 0..count {
                record(
                    core::ptr::from_ref(&INSTANCE).cast(),
                    1,
                    Some(Value::boolean(i % 2 == 0)),
                );
            }
            assert_eq!(evicted(), count.saturating_sub(CAPACITY) as u32);
            for i in count.saturating_sub(CAPACITY)..count {
                let delta = take().unwrap();
                assert_eq!(
                    (delta.sequence, delta.value),
                    (i as u32 + 1, Some(Value::boolean(i % 2 == 0)))
                );
            }
            assert!(take().is_none());
        }
        reset();
        bind(&Owner);
        for (instance, attribute) in [
            (core::ptr::null(), 1),
            (core::ptr::from_ref(&INSTANCE).cast(), 2),
        ] {
            record(instance, attribute, None);
        }
        assert_eq!((sequence(), pending(), evicted()), (0, false, 0));
        let batch = batch();
        record(
            core::ptr::from_ref(&INSTANCE).cast(),
            1,
            Some(Value::boolean(true)),
        );
        assert!(!pending());
        assert!(take().is_none());
        drop(batch);
        assert!(pending());
        assert_eq!(take().unwrap().value, Some(Value::boolean(true)));
        access(|out| out.sequence = u32::MAX - 1);
        for expected in [u32::MAX, 0] {
            record(core::ptr::from_ref(&INSTANCE).cast(), 1, None);
            assert_eq!(take().unwrap().sequence, expected);
        }
        reset();
    }
}
