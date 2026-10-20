//! Native invocation and bounded replay of its result at the appliance owner.
//! A result records the native call's return, not a physical post-state.

use super::frame::{
    decode_arguments, Invocation, InvokeStatus, CALL_ARGUMENT_BYTES, MAX_CALL_ARGUMENTS,
};
use crate::{ir::Value, kernel::appliance::Native, sync::TaskContext};

const RECENT: usize = 8;
#[derive(Clone, Copy)]
struct InvocationRecord {
    caller: u64,
    transformer: u64,
    request: u32,
    arguments: [u8; CALL_ARGUMENT_BYTES],
    length: u8,
    result: InvokeStatus,
}
impl InvocationRecord {
    const EMPTY: Self = Self {
        caller: 0,
        transformer: 0,
        request: 0,
        arguments: [0; CALL_ARGUMENT_BYTES],
        length: 0,
        result: InvokeStatus::Busy,
    };
}

pub(super) struct Invocations {
    records: [InvocationRecord; RECENT],
    next: usize,
}
const _: () = assert!(core::mem::size_of::<Invocations>() <= 768);

impl Invocations {
    pub(super) const fn new() -> Self {
        Self {
            records: [InvocationRecord::EMPTY; RECENT],
            next: 0,
        }
    }

    pub(super) fn invoke(
        &mut self,
        native: &dyn Native,
        context: &TaskContext,
        call: Invocation,
        arguments: &[u8],
    ) -> InvokeStatus {
        if let Some(old) = self
            .records
            .iter()
            .find(|r| r.caller == call.caller && r.request == call.request)
        {
            return if old.transformer == call.transformer
                && &old.arguments[..old.length as usize] == arguments
            {
                old.result
            } else {
                InvokeStatus::ConflictingRequest
            };
        }
        if arguments.len() > CALL_ARGUMENT_BYTES {
            return InvokeStatus::Arguments;
        }
        let mut values = [Value::boolean(false); MAX_CALL_ARGUMENTS];
        let result = match decode_arguments(arguments, &mut values) {
            Some(values) => native.invoke(context, call.transformer, values),
            None => InvokeStatus::Arguments,
        };
        let mut record = InvocationRecord {
            caller: call.caller,
            transformer: call.transformer,
            request: call.request,
            length: arguments.len() as u8,
            result,
            ..InvocationRecord::EMPTY
        };
        record.arguments[..arguments.len()].copy_from_slice(arguments);
        self.records[self.next] = record;
        self.next = (self.next + 1) % RECENT;
        result
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests {
    use super::*;
    use crate::kernel::peer::frame::StateReadResult;
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
        fn invoke(&self, _: &TaskContext, handle: u64, args: &[Value]) -> InvokeStatus {
            assert_eq!((handle, args), (7, &[Value::boolean(true)][..]));
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

    #[test]
    fn replay_identity() {
        let context = unsafe { TaskContext::assume() };
        for (call, args, expected, calls) in [
            (CALL, ON, InvokeStatus::Invoked, 1),
            (
                Invocation { caller: 4, ..CALL },
                ON,
                InvokeStatus::Invoked,
                2,
            ),
            (
                Invocation { request: 4, ..CALL },
                ON,
                InvokeStatus::Invoked,
                2,
            ),
            (
                Invocation {
                    transformer: 8,
                    ..CALL
                },
                ON,
                InvokeStatus::ConflictingRequest,
                1,
            ),
            (CALL, &[1, 0, 0][..], InvokeStatus::ConflictingRequest, 1),
        ] {
            let native = NativeResult(InvokeStatus::Invoked, AtomicUsize::new(0));
            let mut cache = Invocations::new();
            assert_eq!(
                cache.invoke(&native, &context, CALL, ON),
                InvokeStatus::Invoked
            );
            assert_eq!(cache.invoke(&native, &context, call, args), expected);
            assert_eq!(native.1.load(Relaxed), calls);
        }
        for status in [
            InvokeStatus::Invoked,
            InvokeStatus::Busy,
            InvokeStatus::Arguments,
            InvokeStatus::UnknownTransformer,
            InvokeStatus::ApplianceError,
        ] {
            let native = NativeResult(status, AtomicUsize::new(0));
            let mut cache = Invocations::new();
            for _ in 0..2 {
                assert_eq!(cache.invoke(&native, &context, CALL, ON), status);
            }
            assert_eq!(native.1.load(Relaxed), 1, "{status:?} must not imply retry");
        }
    }

    #[test]
    fn argument_admission() {
        let context = unsafe { TaskContext::assume() };
        for (args, retained) in [
            (&[][..], true),
            (&[1, 0][..], true),
            (&[1, 0, 2][..], true),
            (&[1, 0, 1, 0][..], true),
            (&[0; CALL_ARGUMENT_BYTES + 1][..], false),
        ] {
            let native = NativeResult(InvokeStatus::Invoked, AtomicUsize::new(0));
            let mut cache = Invocations::new();
            assert_eq!(
                cache.invoke(&native, &context, CALL, args),
                InvokeStatus::Arguments
            );
            assert_eq!(native.1.load(Relaxed), 0);
            assert_eq!(
                cache.records.iter().filter(|r| r.caller != 0).count(),
                usize::from(retained)
            );
        }
    }

    #[test]
    fn replay_capacity() {
        let context = unsafe { TaskContext::assume() };
        for newer in [RECENT - 1, RECENT, RECENT + 1] {
            let native = NativeResult(InvokeStatus::Invoked, AtomicUsize::new(0));
            let mut cache = Invocations::new();
            cache.invoke(&native, &context, CALL, ON);
            for i in 1..=newer {
                cache.invoke(
                    &native,
                    &context,
                    Invocation {
                        request: CALL.request + i as u32,
                        ..CALL
                    },
                    ON,
                );
            }
            assert_eq!(
                cache.records.iter().filter(|r| r.caller != 0).count(),
                RECENT.min(newer + 1)
            );
            cache.invoke(&native, &context, CALL, ON);
            assert_eq!(
                native.1.load(Relaxed),
                newer + 1 + usize::from(newer >= RECENT)
            );
        }
    }
}
