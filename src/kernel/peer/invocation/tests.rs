extern crate std;
use super::*;
use crate::{ir::Value, kernel::peer::frame::StateReadResult};
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};
struct NativeResult(InvokeStatus, AtomicUsize);
impl Native for NativeResult {
    fn ready(&self) -> bool {
        true
    }
    fn owns(&self, _: *const (), _: u64) -> bool {
        false
    }
    fn read(&self, _: u64) -> StateReadResult {
        unreachable!()
    }
    fn invoke(&self, _: u64, _: &[Value]) -> InvokeStatus {
        self.1.fetch_add(1, Relaxed);
        self.0
    }
}
const CALL: Invocation = Invocation {
    caller: 1,
    owner: 2,
    request: 3,
    transformer: 7,
};
const ON: &[u8] = &[1, 0, 1];
fn setup(status: InvokeStatus) -> (&'static NativeResult, Invocations) {
    crate::port::test_port_reset();

    (
        std::boxed::Box::leak(std::boxed::Box::new(NativeResult(
            status,
            AtomicUsize::new(0),
        ))),
        Invocations::new(),
    )
}
#[test]
fn replay_identity() {
    for status in [
        InvokeStatus::Invoked,
        InvokeStatus::ApplianceError,
        InvokeStatus::Arguments,
        InvokeStatus::Busy,
        InvokeStatus::UnknownTransformer,
    ] {
        let (native, mut cache) = setup(status);
        assert_eq!(cache.submit(native, CALL, ON), Some(status));
        for (call, args, expected) in [
            (CALL, ON, status),
            (
                Invocation {
                    transformer: 8,
                    ..CALL
                },
                ON,
                InvokeStatus::ConflictingRequest,
            ),
            (CALL, &[1, 0, 0][..], InvokeStatus::ConflictingRequest),
        ] {
            assert_eq!(cache.submit(native, call, args), Some(expected));
        }
        for previous in [5, 5, 6, 7] {
            cache.return_via(CALL, previous);
        }
        let done = cache.take_completion().unwrap();
        assert_eq!(
            (done.call, done.status, done.previous),
            (CALL, status, [5, 6])
        );
        assert!(cache.take_completion().is_none());
        assert_eq!(native.1.load(Relaxed), 1);
    }
}
#[test]
fn admission_bounds() {
    for args in [
        &[][..],
        &[1, 0][..],
        &[1, 0, 2][..],
        &[1, 0, 1, 0][..],
        &[0; CALL_ARGUMENT_BYTES + 1][..],
    ] {
        let (native, mut cache) = setup(InvokeStatus::Invoked);
        assert_eq!(
            cache.submit(native, CALL, args),
            Some(InvokeStatus::Arguments)
        );
        assert_eq!(native.1.load(Relaxed), 0);
    }
}
#[test]
fn receipt_capacity() {
    let (native, mut cache) = setup(InvokeStatus::Invoked);
    for request in 0..RECENT as u32 {
        let call = Invocation { request, ..CALL };
        assert_eq!(cache.submit(native, call, ON), Some(InvokeStatus::Invoked));
        cache.return_via(call, 5);
    }
    assert_eq!(
        cache.submit(
            native,
            Invocation {
                request: 100,
                ..CALL
            },
            ON
        ),
        Some(InvokeStatus::Busy)
    );
    assert_eq!(native.1.load(Relaxed), RECENT);
    assert!(cache.take_completion().is_some());
    assert_eq!(
        cache.submit(
            native,
            Invocation {
                request: 100,
                ..CALL
            },
            ON
        ),
        Some(InvokeStatus::Invoked)
    );
    assert_eq!(native.1.load(Relaxed), RECENT + 1);
}
