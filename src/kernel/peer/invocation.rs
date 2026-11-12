//! Owner-local admission and result replay. Running calls cannot be evicted.
use super::frame::{Invocation, InvokeStatus, CALL_ARGUMENT_BYTES};
use crate::{ir::Value, kernel::appliance::Native};
use lattice_peer_wire::{scalar::decode_arguments, MAX_CALL_ARGUMENTS};
fn invoke_bytes(native: &'static dyn Native, transformer: u64, arguments: &[u8]) -> InvokeStatus {
    let mut values = [Value::boolean(false); MAX_CALL_ARGUMENTS];
    match decode_arguments(arguments, &mut values) {
        Some(values) => native.invoke(transformer, values),
        None => InvokeStatus::Arguments,
    }
}

const RECENT: usize = 8;
#[derive(Clone, Copy)]
enum ResultState {
    Empty,
    Running,
    Complete(InvokeStatus),
}
#[derive(Clone, Copy)]
struct InvocationRecord {
    call: Invocation,
    arguments: [u8; CALL_ARGUMENT_BYTES],
    length: u8,
    result: ResultState,
    previous: [u64; 2],
    pending: bool,
}
impl InvocationRecord {
    const EMPTY: Self = Self {
        call: Invocation {
            caller: 0,
            owner: 0,
            request: 0,
            transformer: 0,
        },
        arguments: [0; CALL_ARGUMENT_BYTES],
        length: 0,
        result: ResultState::Empty,
        previous: [0; 2],
        pending: false,
    };
}
pub(super) struct Completion {
    pub call: Invocation,
    pub status: InvokeStatus,
    pub previous: [u64; 2],
}
pub(super) struct Invocations {
    records: [InvocationRecord; RECENT],
    next: usize,
}
const _: () = assert!(core::mem::size_of::<Invocations>() <= 1280);
impl Invocations {
    pub(super) const fn new() -> Self {
        Self {
            records: [InvocationRecord::EMPTY; RECENT],
            next: 0,
        }
    }
    pub(super) fn submit(
        &mut self,
        native: &'static dyn Native,
        call: Invocation,
        arguments: &[u8],
    ) -> Option<InvokeStatus> {
        if let Some(old) = self.records.iter().find(|r| {
            !matches!(r.result, ResultState::Empty)
                && r.call.caller == call.caller
                && r.call.request == call.request
        }) {
            if old.call != call || &old.arguments[..old.length as usize] != arguments {
                return Some(InvokeStatus::ConflictingRequest);
            }
            return match old.result {
                ResultState::Complete(result) => Some(result),
                _ => None,
            };
        }
        if arguments.len() > CALL_ARGUMENT_BYTES {
            return Some(InvokeStatus::Arguments);
        }
        let Some(index) = (0..RECENT).map(|n| (self.next + n) % RECENT).find(|&i| {
            !self.records[i].pending && !matches!(self.records[i].result, ResultState::Running)
        }) else {
            return Some(InvokeStatus::Busy);
        };
        let result = ResultState::Running;
        let mut record = InvocationRecord {
            call,
            length: arguments.len() as u8,
            result,
            ..InvocationRecord::EMPTY
        };
        record.arguments[..arguments.len()].copy_from_slice(arguments);
        self.records[index] = record;
        self.next = (index + 1) % RECENT;
        let status = invoke_bytes(native, call.transformer, arguments);
        self.records[index].result = ResultState::Complete(status);
        Some(status)
    }

    pub(super) fn return_via(&mut self, call: Invocation, previous: u64) {
        let Some(record) = self.records.iter_mut().find(|r| r.call == call) else {
            return;
        };
        record.pending = true;
        if !record.previous.contains(&previous) {
            if let Some(slot) = record.previous.iter_mut().find(|p| **p == 0) {
                *slot = previous;
            }
        }
    }
    pub(super) fn result(&self, call: Invocation) -> Option<InvokeStatus> {
        self.records.iter().find_map(|r| {
            if r.call == call {
                if let ResultState::Complete(status) = r.result {
                    Some(status)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    pub(super) fn pending(&self) -> bool {
        self.records.iter().any(|r| r.pending)
    }
    pub(super) fn take_completion(&mut self) -> Option<Completion> {
        for record in &mut self.records {
            if record.pending {
                if let ResultState::Complete(status) = record.result {
                    let previous = record.previous;
                    record.previous = [0; 2];
                    record.pending = false;
                    return Some(Completion {
                        call: record.call,
                        status,
                        previous,
                    });
                }
            }
        }
        None
    }
}
// One owner shared by the radio ISR and peer task. No task holds a borrow across
// an interrupt; native callbacks cannot access this private ledger.
static mut INVOCATIONS: Invocations = Invocations::new();
pub(super) fn access<R>(f: impl FnOnce(&mut Invocations) -> R) -> R {
    crate::kernel::state::atomic(|| f(unsafe { &mut *core::ptr::addr_of_mut!(INVOCATIONS) }))
}
#[cfg(test)]
pub(super) fn reset() {
    access(|calls| *calls = Invocations::new());
}

#[cfg(all(test, feature = "port-test"))]
mod tests;
