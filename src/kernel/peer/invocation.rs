//! Owner-local admission and result replay. Running calls cannot be evicted.
use super::frame::{Invocation, InvokeStatus, CALL_ARGUMENT_BYTES};
use crate::{
    kernel::appliance::{
        execution::{self, Ticket},
        Native,
    },
    sync::TaskContext,
};

const RECENT: usize = 8;
#[derive(Clone, Copy)]
enum ResultState {
    Empty,
    Running(Ticket),
    Complete(InvokeStatus),
}
#[derive(Clone, Copy)]
struct InvocationRecord {
    call: Invocation,
    arguments: [u8; CALL_ARGUMENT_BYTES],
    length: u8,
    result: ResultState,
    previous: [u64; 2],
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
        context: &TaskContext,
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
        let Some(index) = (0..RECENT)
            .map(|n| (self.next + n) % RECENT)
            .find(|&i| !matches!(self.records[i].result, ResultState::Running(_)))
        else {
            return Some(InvokeStatus::Busy);
        };
        let result = match execution::submit(native, context, call.transformer, arguments) {
            Ok(ticket) => ResultState::Running(ticket),
            Err(error) => ResultState::Complete(error),
        };
        let mut record = InvocationRecord {
            call,
            length: arguments.len() as u8,
            result,
            ..InvocationRecord::EMPTY
        };
        record.arguments[..arguments.len()].copy_from_slice(arguments);
        self.records[index] = record;
        self.next = (index + 1) % RECENT;
        match result {
            ResultState::Complete(result) => Some(result),
            _ => None,
        }
    }
    pub(super) fn return_via(&mut self, call: Invocation, previous: u64) {
        let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.call == call && matches!(r.result, ResultState::Running(_)))
        else {
            return;
        };
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
    pub(super) fn take_completion(&mut self) -> Option<Completion> {
        for record in &mut self.records {
            if let ResultState::Running(ticket) = record.result {
                if let Some(status) = execution::take_result(ticket) {
                    record.result = ResultState::Complete(status);
                    return Some(Completion {
                        call: record.call,
                        status,
                        previous: record.previous,
                    });
                }
            }
        }
        None
    }
}
#[cfg(all(test, feature = "port-test"))]
mod tests;
