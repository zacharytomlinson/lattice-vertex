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
    fn read(&self, _: &TaskContext, _: u64) -> StateReadResult {
        unreachable!()
    }
    fn invoke(&self, _: &TaskContext, _: u64, _: &[Value]) -> InvokeStatus {
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
fn setup(status: InvokeStatus) -> (TaskContext, &'static NativeResult, Invocations) {
    crate::port::test_port_reset();
    execution::reset();
    (
        unsafe { TaskContext::assume() },
        std::boxed::Box::leak(std::boxed::Box::new(NativeResult(
            status,
            AtomicUsize::new(0),
        ))),
        Invocations::new(),
    )
}
fn finish(cache: &mut Invocations, context: &TaskContext) {
    execution::run_ready(context);
    while cache.take_completion().is_some() {}
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
        let (context, native, mut cache) = setup(status);
        assert_eq!(cache.submit(native, &context, CALL, ON), None);
        assert_eq!(
            native.1.load(Relaxed),
            0,
            "admission must not call hardware inline"
        );
        for (call, args, expected) in [
            (CALL, ON, None),
            (
                Invocation {
                    transformer: 8,
                    ..CALL
                },
                ON,
                Some(InvokeStatus::ConflictingRequest),
            ),
            (CALL, &[1, 0, 0][..], Some(InvokeStatus::ConflictingRequest)),
        ] {
            assert_eq!(cache.submit(native, &context, call, args), expected);
        }
        for direction in [5, 5, 6, 7] {
            cache.return_via(CALL, direction);
        }
        execution::run_ready(&context);
        let done = cache.take_completion().unwrap();
        assert_eq!(
            (done.call, done.status, done.previous),
            (CALL, status, [5, 6])
        );
        assert!(cache.take_completion().is_none());
        assert_eq!(cache.submit(native, &context, CALL, ON), Some(status));
        assert_eq!(native.1.load(Relaxed), 1);
    }
}
#[test]
fn admission_bounds() {
    for count in [
        0,
        lattice_peer_wire::MAX_CALL_ARGUMENTS,
        crate::configLATTICE_PROGRAM_ARGUMENTS,
        crate::configLATTICE_PROGRAM_ARGUMENTS + 1,
    ] {
        let (context, native, _) = setup(InvokeStatus::Invoked);
        let values =
            std::vec![Value::integer(crate::ir::Integer::U64,u64::MAX as i128).unwrap();count];
        let result = execution::submit_values(native, &context, CALL.transformer, &values);
        if count > crate::configLATTICE_PROGRAM_ARGUMENTS {
            assert_eq!(result, Err(InvokeStatus::Arguments));
        } else {
            let ticket = result.unwrap();
            assert_eq!(native.1.load(Relaxed), 0);
            execution::run_ready(&context);
            assert_eq!(execution::take_result(ticket), Some(InvokeStatus::Invoked));
            assert_eq!(native.1.load(Relaxed), 1);
        }
    }
    for args in [
        &[][..],
        &[1, 0][..],
        &[1, 0, 2][..],
        &[1, 0, 1, 0][..],
        &[0; CALL_ARGUMENT_BYTES + 1][..],
    ] {
        let (context, native, mut cache) = setup(InvokeStatus::Invoked);
        let immediate = cache.submit(native, &context, CALL, args);
        if args.len() > CALL_ARGUMENT_BYTES {
            assert_eq!(immediate, Some(InvokeStatus::Arguments));
        } else {
            assert_eq!(immediate, None);
            finish(&mut cache, &context);
            assert_eq!(cache.result(CALL), Some(InvokeStatus::Arguments));
        }
        assert_eq!(native.1.load(Relaxed), 0);
    }
    let (context, native, mut cache) = setup(InvokeStatus::Invoked);
    for n in 0..crate::configLATTICE_NATIVE_TASKS {
        assert_eq!(
            cache.submit(
                native,
                &context,
                Invocation {
                    request: n as u32 + 10,
                    ..CALL
                },
                ON
            ),
            None
        );
    }
    // Completed Busy records may churn; admitted calls and their payload stay resident.
    for n in 0..RECENT * 2 {
        assert_eq!(
            cache.submit(
                native,
                &context,
                Invocation {
                    request: n as u32 + 100,
                    ..CALL
                },
                ON
            ),
            Some(InvokeStatus::Busy)
        );
    }
    finish(&mut cache, &context);
    for n in 0..crate::configLATTICE_NATIVE_TASKS {
        assert_eq!(
            cache.result(Invocation {
                request: n as u32 + 10,
                ..CALL
            }),
            Some(InvokeStatus::Invoked)
        );
    }
    assert_eq!(native.1.load(Relaxed), crate::configLATTICE_NATIVE_TASKS);
}
#[test]
fn replay_capacity() {
    for newer in [RECENT - 1, RECENT, RECENT + 1] {
        let (context, native, mut cache) = setup(InvokeStatus::Invoked);
        for n in 0..=newer {
            assert_eq!(
                cache.submit(
                    native,
                    &context,
                    Invocation {
                        request: CALL.request + n as u32,
                        ..CALL
                    },
                    ON
                ),
                None
            );
            finish(&mut cache, &context);
        }
        let result = cache.submit(native, &context, CALL, ON);
        assert_eq!(
            result,
            if newer < RECENT {
                Some(InvokeStatus::Invoked)
            } else {
                None
            }
        );
        finish(&mut cache, &context);
        assert_eq!(
            native.1.load(Relaxed),
            newer + 1 + usize::from(newer >= RECENT)
        );
    }
}
