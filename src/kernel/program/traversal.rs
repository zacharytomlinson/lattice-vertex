//! Bounded visits and complete receiver-reserved handoffs. Each visit owns its
//! compiler-sized working extent until completion or acknowledged transfer.
use super::{
    evaluation::{self, Values},
    Configuration,
};
use crate::{
    ir::Value,
    kernel::{
        appliance::{execution, Native},
        peer::frame::{InvokeStatus, StateReadResult},
    },
    sync::TaskContext,
};
use lattice_peer_wire::program::{
    execution::{Cause, Entry, Request, Ticket, CHUNK_BYTES},
    transfer::{Reply, Status, DATA_BYTES, REQUEST_BYTES},
    Definition, Link, Operation, Reference,
};
use sha2::{Digest, Sha256};

const COUNT: usize = crate::configLATTICE_TRAVERSALS;
const BYTES: usize = crate::configLATTICE_TRAVERSAL_BYTES;
const RECEIPTS: usize = COUNT * 2;
const RECEIVING_MS: u32 = 30_000;
const RECEIPT_MS: u32 = 60_000;
const _: () = assert!(COUNT > 0 && RECEIPTS <= u16::MAX as usize && BYTES <= u16::MAX as usize);
pub(super) fn fits(d: Definition<'_>) -> Result<(), Status> {
    if d.carried_bytes() as usize + d.scratch_bytes() as usize > BYTES {
        return Err(Status::Bounds);
    }
    for r in d.records() {
        if let Operation::Invoke { arguments, .. } = r.map_err(super::status)?.operation {
            if arguments.len() > crate::configLATTICE_PROGRAM_ARGUMENTS {
                return Err(Status::Bounds);
            }
        }
    }
    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Session,
    Reserve,
    Data(u16),
    Commit,
    Release,
}
#[derive(Clone, Copy)]
struct Sending {
    target: Reference,
    upstream: bool,
    sequence: u32,
    stage: Stage,
    ticket: Ticket,
    waiting: Option<u16>,
    sent: u32,
    due: u32,
    backoff: u8,
}
#[derive(Clone, Copy)]
enum Phase {
    Reserved,
    Upstream,
    Forward,
    Calling {
        ticket: execution::Ticket,
        success: Link,
        failure: Link,
    },
    Sending(Sending),
}
#[derive(Clone, Copy)]
struct Visit {
    entry: Entry,
    cause: Cause,
    start: u16,
    length: u16,
    carried: u16,
    phase: Phase,
}
#[derive(Clone, Copy)]
struct Receipt {
    caller: u64,
    sender_epoch: u64,
    sequence: u32,
    cause: Cause,
    entry: Entry,
    upstream: bool,
    length: u16,
    received: u16,
    visit: usize,
    digest: Option<[u8; 32]>,
    touched: u32,
}
struct ReceiptSlot {
    generation: u32,
    receipt: Option<Receipt>,
}
impl ReceiptSlot {
    const fn new() -> Self {
        Self {
            generation: 0,
            receipt: None,
        }
    }
}
#[derive(Clone, Copy, Default)]
pub(super) struct Statistics {
    pub admitted: u32,
    pub completed: u32,
    pub faults: u32,
    pub uncertain: u32,
    pub last_status: u8,
    pub last_definition: u16,
    pub last_record: u16,
}
pub(super) struct Traversals {
    bytes: [u8; BYTES],
    visits: [Option<Visit>; COUNT],
    receipts: [ReceiptSlot; RECEIPTS],
    sequence: u32,
    pub local: u64,
    pub epoch: u64,
    pub statistics: Statistics,
    recovery: u16,
    recovered: bool,
    turn: usize,
}
impl Traversals {
    pub const fn new() -> Self {
        Self {
            bytes: [0; BYTES],
            visits: [None; COUNT],
            receipts: [const { ReceiptSlot::new() }; RECEIPTS],
            sequence: 0,
            local: 0,
            epoch: 0,
            statistics: Statistics {
                admitted: 0,
                completed: 0,
                faults: 0,
                uncertain: 0,
                last_status: 0,
                last_definition: 0,
                last_record: 0,
            },
            recovery: 0,
            recovered: false,
            turn: 0,
        }
    }
    pub fn pending(&self) -> bool {
        self.visits.iter().any(Option::is_some)
            || self.receipts.iter().any(|s| s.receipt.is_some())
            || (!self.recovered && self.epoch != 0)
    }
    fn next(&mut self) -> Result<u32, Status> {
        self.sequence = self.sequence.checked_add(1).ok_or(Status::Bounds)?;
        Ok(self.sequence)
    }
    fn extent(&self, length: usize, exclude: Option<usize>) -> Result<usize, Status> {
        let mut at = 0;
        loop {
            if at + length > BYTES {
                return Err(Status::Busy);
            }
            let next = self
                .visits
                .iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != exclude)
                .filter_map(|(_, v)| *v)
                .filter(|v| {
                    v.length != 0
                        && at < v.start as usize + v.length as usize
                        && at + length > v.start as usize
                })
                .map(|v| v.start as usize + v.length as usize)
                .max();
            match next {
                Some(end) => at = end,
                None => return Ok(at),
            }
        }
    }
    fn reserve(
        &mut self,
        programs: &Configuration,
        entry: Entry,
        cause: Cause,
        upstream: bool,
        length: u16,
    ) -> Result<usize, Status> {
        let d = programs
            .part(entry.definition, entry.generation)?
            .definition;
        fits(d)?;
        if !matches!(
            d.record(entry.record).map_err(super::status)?.operation,
            Operation::Entry { .. }
        ) {
            return Err(Status::Binding);
        }
        if length != if upstream { 0 } else { d.carried_bytes() } {
            return Err(Status::Bounds);
        }
        let capacity = if upstream {
            0
        } else {
            length as usize + d.scratch_bytes() as usize
        };
        let slot = self
            .visits
            .iter()
            .position(Option::is_none)
            .ok_or(Status::Busy)?;
        let start = self.extent(capacity, None)?;
        self.bytes[start..start + capacity].fill(0);
        self.visits[slot] = Some(Visit {
            entry,
            cause,
            start: start as u16,
            length: capacity as u16,
            carried: length,
            phase: Phase::Reserved,
        });
        Ok(slot)
    }
    pub fn activate(&mut self, programs: &Configuration, entry: Entry) -> Result<(), Status> {
        if self.epoch == 0 {
            return Err(Status::Busy);
        }
        let cause = Cause {
            owner: self.local,
            epoch: self.epoch,
            sequence: self.next()?,
        };
        let slot = self.reserve(programs, entry, cause, true, 0)?;
        self.visits[slot].as_mut().unwrap().phase = Phase::Upstream;
        self.statistics.admitted = self.statistics.admitted.wrapping_add(1);
        Ok(())
    }
    fn fail(&mut self, index: usize, status: Status) {
        let v = self.visits[index].take().unwrap();
        self.fault(v.entry, status);
    }
    pub fn fault(&mut self, entry: Entry, status: Status) {
        self.statistics.faults = self.statistics.faults.wrapping_add(1);
        self.statistics.last_status = status as u8;
        self.statistics.last_definition = entry.definition;
        self.statistics.last_record = entry.record;
        if status == Status::Stale {
            self.statistics.uncertain = self.statistics.uncertain.wrapping_add(1);
        }
    }
    fn finish(&mut self, index: usize) {
        self.visits[index] = None;
        self.statistics.completed = self.statistics.completed.wrapping_add(1);
    }
    fn enter_forward(&mut self, index: usize, d: Definition<'_>) -> Result<(), Status> {
        let mut v = self.visits[index].unwrap();
        let n = d.carried_bytes() as usize + d.scratch_bytes() as usize;
        let at = self.extent(n, Some(index))?;
        if v.carried != 0 && v.carried != d.carried_bytes() {
            return Err(Status::Bounds);
        }
        self.bytes
            .copy_within(v.start as usize..v.start as usize + v.carried as usize, at);
        self.bytes[at + v.carried as usize..at + n].fill(0);
        v.start = at as u16;
        v.length = n as u16;
        v.carried = d.carried_bytes();
        v.phase = Phase::Forward;
        self.visits[index] = Some(v);
        Ok(())
    }
    fn follow(
        &mut self,
        index: usize,
        link: Link,
        upstream: bool,
        programs: &Configuration,
    ) -> Result<(), Status> {
        match link {
            Link::Stop => self.finish(index),
            Link::Local(record) => {
                let v = self.visits[index].as_mut().unwrap();
                v.entry.record = record;
                v.phase = if upstream {
                    Phase::Upstream
                } else {
                    Phase::Forward
                };
            }
            Link::Remote(target) if target.owner == self.local => {
                let d = programs
                    .part(target.definition, target.generation)?
                    .definition;
                fits(d)?;
                if !matches!(
                    d.record(target.record).map_err(super::status)?.operation,
                    Operation::Entry { .. }
                ) {
                    return Err(Status::Binding);
                }
                if !upstream {
                    self.enter_forward(index, d)?;
                }
                let v = self.visits[index].as_mut().unwrap();
                v.entry = Entry {
                    definition: target.definition,
                    generation: target.generation,
                    record: target.record,
                };
                v.phase = if upstream {
                    Phase::Upstream
                } else {
                    Phase::Forward
                };
            }
            Link::Remote(target) => {
                let sequence = self.next()?;
                self.visits[index].as_mut().unwrap().phase = Phase::Sending(Sending {
                    target,
                    upstream,
                    sequence,
                    stage: Stage::Session,
                    ticket: Ticket {
                        epoch: 0,
                        slot: 0,
                        generation: 0,
                    },
                    waiting: None,
                    sent: 0,
                    due: 0,
                    backoff: 0,
                });
            }
        }
        Ok(())
    }
    pub fn poll(
        &mut self,
        programs: &mut Configuration,
        native: &'static dyn Native,
        context: &TaskContext,
        now: u32,
    ) {
        if self.epoch == 0 || self.local == 0 {
            return;
        }
        for slot in &mut self.receipts {
            if let Some(r) = slot.receipt {
                if now.wrapping_sub(r.touched)
                    >= if r.digest.is_some() {
                        RECEIPT_MS
                    } else {
                        RECEIVING_MS
                    }
                {
                    if r.digest.is_none() {
                        self.visits[r.visit] = None;
                    }
                    slot.receipt = None;
                }
            }
        }
        if crate::kernel::change::committed() {
            if let Some(entry) = programs.initial() {
                match self.activate(programs, entry) {
                    Err(Status::Busy) => {}
                    result => {
                        if let Err(error) = result {
                            self.fault(entry, error);
                        }
                        programs.bytes[entry.definition as usize + 2] &= 1;
                    }
                }
            }
            if !self.recovered {
                match programs.recovery(self.recovery) {
                    Some((cursor, entry)) => match self.activate(programs, entry) {
                        Err(Status::Busy) => {}
                        result => {
                            if let Err(error) = result {
                                self.fault(entry, error);
                            }
                            self.recovery = cursor;
                        }
                    },
                    None => self.recovered = true,
                }
            }
        }
        let index = self.turn;
        self.turn = (self.turn + 1) % COUNT;
        // The validated local graph has at most MAX_RECORDS forward records.
        // One visit receives a bounded turn; native work runs in RTOS call tasks.
        for _ in 0..super::validation::MAX_RECORDS {
            let Some(v) = self.visits[index] else { break };
            let result = self.step(index, v, programs, native, context);
            match result {
                Ok(true) => {}
                Ok(false) => break,
                Err(Status::Busy | Status::Unavailable) => break,
                Err(e) => {
                    self.fail(index, e);
                    break;
                }
            }
        }
    }
    fn step(
        &mut self,
        index: usize,
        v: Visit,
        programs: &Configuration,
        native: &'static dyn Native,
        context: &TaskContext,
    ) -> Result<bool, Status> {
        if let Phase::Calling {
            ticket,
            success,
            failure,
        } = v.phase
        {
            let Some(status) = execution::take_result(ticket) else {
                return Ok(false);
            };
            self.follow(
                index,
                if status == InvokeStatus::Invoked {
                    success
                } else {
                    failure
                },
                false,
                programs,
            )?;
            return Ok(true);
        }
        if matches!(v.phase, Phase::Reserved | Phase::Sending(_)) {
            return Ok(false);
        }
        let d = programs
            .part(v.entry.definition, v.entry.generation)?
            .definition;
        let op = d.record(v.entry.record).map_err(super::status)?.operation;
        // The head is the admission gate even if a foreign forward link names it.
        // Visits which have already passed it are unaffected by later disablement.
        if d.head() == Some(v.entry.record) && !programs.enabled(v.entry.definition) {
            self.finish(index);
            return Ok(false);
        }
        if matches!(v.phase, Phase::Upstream) {
            let Operation::Entry { upstream, next, .. } = op else {
                return Err(Status::Binding);
            };
            if d.head() == Some(v.entry.record) {
                self.enter_forward(index, d)?;
                self.follow(index, next, false, programs)?;
            } else {
                self.follow(index, upstream, true, programs)?;
            }
            return Ok(true);
        }
        let (carried, scratch) = self.bytes[v.start as usize..v.start as usize + v.length as usize]
            .split_at_mut(v.carried as usize);
        let mut values = Values { carried, scratch };
        let next = match op {
            Operation::Temporal { yes, no, .. } => {
                let source = v.entry.definition + super::PREFIX as u16 + v.entry.record;
                if super::runtime::temporal_value(source).ok_or(Status::Unavailable)? {
                    yes
                } else {
                    no
                }
            }
            Operation::Entry { inputs, next, .. } => {
                for input in inputs.iter() {
                    values.read(input).map_err(super::status)?;
                }
                next
            }
            Operation::Read {
                state,
                output,
                next,
            } => {
                let value = match native.read(context, state) {
                    StateReadResult::Value { value: Some(v), .. } => v,
                    StateReadResult::Value { value: None, .. } => return Err(Status::Unavailable),
                    StateReadResult::Busy => return Err(Status::Busy),
                    _ => return Err(Status::Binding),
                };
                values.write(output, value).map_err(super::status)?;
                next
            }
            Operation::Unary {
                operation,
                input,
                output,
                next,
            } => {
                let value =
                    evaluation::unary(operation, values.read(input).map_err(super::status)?)
                        .map_err(super::status)?;
                values.write(output, value).map_err(super::status)?;
                next
            }
            Operation::Binary {
                operation,
                left,
                right,
                output,
                next,
            } => {
                let value = evaluation::binary(
                    operation,
                    values.read(left).map_err(super::status)?,
                    values.read(right).map_err(super::status)?,
                )
                .map_err(super::status)?;
                values.write(output, value).map_err(super::status)?;
                next
            }
            Operation::Test {
                operation,
                left,
                right,
                yes,
                no,
            } => {
                if evaluation::binary(
                    operation,
                    values.read(left).map_err(super::status)?,
                    values.read(right).map_err(super::status)?,
                )
                .map_err(super::status)?
                .as_bool()
                .ok_or(Status::Invalid)?
                {
                    yes
                } else {
                    no
                }
            }
            Operation::Branch { input, yes, no } => {
                if values
                    .read(input)
                    .map_err(super::status)?
                    .as_bool()
                    .ok_or(Status::Invalid)?
                {
                    yes
                } else {
                    no
                }
            }
            Operation::Invoke {
                transformer,
                arguments,
                success,
                failure,
            } => {
                let mut args = [Value::boolean(false); crate::configLATTICE_PROGRAM_ARGUMENTS];
                for (at, arg) in arguments.iter().enumerate() {
                    args[at] = values.read(arg).map_err(super::status)?;
                }
                let ticket = execution::submit_values(
                    native,
                    context,
                    transformer,
                    &args[..arguments.len()],
                )
                .map_err(|e| {
                    if e == InvokeStatus::Busy {
                        Status::Busy
                    } else {
                        Status::Binding
                    }
                })?;
                self.visits[index].as_mut().unwrap().phase = Phase::Calling {
                    ticket,
                    success,
                    failure,
                };
                return Ok(false);
            }
        };
        self.follow(index, next, false, programs)?;
        Ok(true)
    }
    fn receipt(&self, caller: u64, t: Ticket) -> Result<Receipt, Status> {
        if t.epoch != self.epoch {
            return Err(Status::Stale);
        }
        let slot = self.receipts.get(t.slot as usize).ok_or(Status::Stale)?;
        if slot.generation != t.generation {
            return Err(Status::Stale);
        }
        slot.receipt
            .filter(|r| r.caller == caller)
            .ok_or(Status::Stale)
    }
    pub fn receive<'a>(
        &mut self,
        programs: &Configuration,
        caller: u64,
        request: Request<'_>,
        now: u32,
        out: &'a mut [u8; DATA_BYTES],
    ) -> Reply<'a> {
        if self.epoch == 0 {
            return Reply::status(Status::Busy);
        }
        match self.handle(programs, caller, request, now, out) {
            Ok(r) => r,
            Err(e) => Reply::status(e),
        }
    }
    fn handle<'a>(
        &mut self,
        programs: &Configuration,
        caller: u64,
        request: Request<'_>,
        now: u32,
        out: &'a mut [u8; DATA_BYTES],
    ) -> Result<Reply<'a>, Status> {
        match request {
            Request::Session => {
                out[..8].copy_from_slice(&self.epoch.to_le_bytes());
                out[8..10].copy_from_slice(&(BYTES as u16).to_le_bytes());
                out[10..12].copy_from_slice(&(COUNT as u16).to_le_bytes());
                out[12..14].copy_from_slice(
                    &(crate::configLATTICE_PROGRAM_ARGUMENTS as u16).to_le_bytes(),
                );
                Ok(Reply {
                    bytes: &out[..14],
                    ..Reply::status(Status::Ok)
                })
            }
            Request::Statistics => {
                let s = self.statistics;
                for (i, v) in [
                    s.admitted,
                    s.completed,
                    s.faults,
                    s.uncertain,
                    super::activation::overflow(),
                ]
                .into_iter()
                .enumerate()
                {
                    out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                out[20] = s.last_status;
                out[21..23].copy_from_slice(&s.last_definition.to_le_bytes());
                out[23..25].copy_from_slice(&s.last_record.to_le_bytes());
                Ok(Reply {
                    bytes: &out[..25],
                    ..Reply::status(Status::Ok)
                })
            }
            Request::Reserve {
                epoch,
                sender_epoch,
                sequence,
                cause,
                entry,
                upstream,
                length,
            } => {
                if epoch != self.epoch {
                    return Err(Status::Stale);
                }
                if sender_epoch == 0
                    || sequence == 0
                    || cause.epoch == 0
                    || cause.sequence == 0
                    || cause.owner == 0
                    || cause.owner == u64::MAX
                {
                    return Err(Status::Invalid);
                }
                if let Some((i, s)) = self.receipts.iter().enumerate().find(|(_, s)| {
                    s.receipt.is_some_and(|r| {
                        r.caller == caller
                            && r.sender_epoch == sender_epoch
                            && r.sequence == sequence
                    })
                }) {
                    let r = s.receipt.unwrap();
                    if (r.entry, r.cause, r.upstream, r.length) != (entry, cause, upstream, length)
                    {
                        return Err(Status::Conflict);
                    }
                    return Ok(Reply {
                        offset: i as u16,
                        generation: s.generation,
                        accepted: r.received,
                        total: r.length,
                        ..Reply::status(if r.digest.is_some() {
                            Status::Accepted
                        } else {
                            Status::Receiving
                        })
                    });
                }
                let slot = self
                    .receipts
                    .iter()
                    .position(|s| s.receipt.is_none() && s.generation < u32::MAX)
                    .ok_or(Status::Busy)?;
                let visit = self.reserve(programs, entry, cause, upstream, length)?;
                let s = &mut self.receipts[slot];
                s.generation += 1;
                s.receipt = Some(Receipt {
                    caller,
                    sender_epoch,
                    sequence,
                    cause,
                    entry,
                    upstream,
                    length,
                    received: 0,
                    visit,
                    digest: None,
                    touched: now,
                });
                Ok(Reply {
                    offset: slot as u16,
                    generation: s.generation,
                    total: length,
                    ..Reply::status(Status::Receiving)
                })
            }
            Request::Data {
                ticket,
                offset,
                bytes,
            } => {
                let mut r = self.receipt(caller, ticket)?;
                if r.digest.is_some() {
                    return Err(Status::Conflict);
                }
                let v = self.visits[r.visit].ok_or(Status::Stale)?;
                let end = offset as usize + bytes.len();
                if end > r.length as usize {
                    return Err(Status::Bounds);
                }
                let start = v.start as usize + offset as usize;
                if end <= r.received as usize {
                    if self.bytes[start..start + bytes.len()] != *bytes {
                        return Err(Status::Conflict);
                    }
                } else if offset != r.received {
                    return Err(Status::Conflict);
                } else {
                    self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
                    r.received = end as u16;
                }
                r.touched = now;
                self.receipts[ticket.slot as usize].receipt = Some(r);
                Ok(Reply {
                    offset: ticket.slot,
                    generation: ticket.generation,
                    accepted: r.received,
                    total: r.length,
                    ..Reply::status(Status::Receiving)
                })
            }
            Request::Commit { ticket, digest } => {
                let mut r = self.receipt(caller, ticket)?;
                if let Some(saved) = r.digest {
                    if saved != digest {
                        return Err(Status::Conflict);
                    }
                } else {
                    if r.received != r.length {
                        return Err(Status::Invalid);
                    }
                    let v = self.visits[r.visit].ok_or(Status::Stale)?;
                    let hash: [u8; 32] = Sha256::digest(
                        &self.bytes[v.start as usize..v.start as usize + r.length as usize],
                    )
                    .into();
                    if hash != digest {
                        return Err(Status::Conflict);
                    }
                    if !r.upstream {
                        let d = programs
                            .part(r.entry.definition, r.entry.generation)?
                            .definition;
                        let Operation::Entry { inputs, .. } =
                            d.record(r.entry.record).map_err(super::status)?.operation
                        else {
                            return Err(Status::Binding);
                        };
                        let (carried, scratch) = self.bytes
                            [v.start as usize..v.start as usize + v.length as usize]
                            .split_at_mut(v.carried as usize);
                        let values = Values { carried, scratch };
                        for operand in inputs.iter() {
                            values.read(operand).map_err(super::status)?;
                        }
                    }
                    self.visits[r.visit].as_mut().unwrap().phase = if r.upstream {
                        Phase::Upstream
                    } else {
                        Phase::Forward
                    };
                    r.digest = Some(digest);
                    r.touched = now;
                    self.receipts[ticket.slot as usize].receipt = Some(r);
                    self.statistics.admitted = self.statistics.admitted.wrapping_add(1);
                }
                Ok(Reply {
                    offset: ticket.slot,
                    generation: ticket.generation,
                    accepted: r.received,
                    total: r.length,
                    ..Reply::status(Status::Accepted)
                })
            }
            Request::Release { ticket } => {
                let r = self.receipt(caller, ticket)?;
                if r.digest.is_none() {
                    return Err(Status::Conflict);
                }
                self.receipts[ticket.slot as usize].receipt = None;
                Ok(Reply::status(Status::Ok))
            }
            _ => Err(Status::Unsupported),
        }
    }
    /// One outstanding radio request at a time; local/native visits still overlap.
    pub fn outgoing(
        &mut self,
        now: u32,
        out: &mut [u8; REQUEST_BYTES],
    ) -> Option<(usize, u64, usize)> {
        if self.awaiting().is_some() {
            return None;
        }
        for (index, v) in self
            .visits
            .iter_mut()
            .enumerate()
            .filter_map(|(i, v)| v.as_mut().map(|v| (i, v)))
        {
            let Phase::Sending(s) = &mut v.phase else {
                continue;
            };
            if s.waiting.is_some() {
                return None;
            }
            if s.due != 0 && now.wrapping_sub(s.due) >= 1 << 31 {
                continue;
            }
            let request = match s.stage {
                Stage::Session => Request::Session,
                Stage::Reserve => Request::Reserve {
                    epoch: s.ticket.epoch,
                    sender_epoch: self.epoch,
                    sequence: s.sequence,
                    cause: v.cause,
                    entry: Entry {
                        definition: s.target.definition,
                        generation: s.target.generation,
                        record: s.target.record,
                    },
                    upstream: s.upstream,
                    length: if s.upstream { 0 } else { v.carried },
                },
                Stage::Data(offset) => {
                    let length = if s.upstream { 0 } else { v.carried };
                    let end = (offset as usize + CHUNK_BYTES).min(length as usize);
                    if end == offset as usize {
                        s.stage = Stage::Commit;
                        return self.outgoing(now, out);
                    }
                    Request::Data {
                        ticket: s.ticket,
                        offset,
                        bytes: &self.bytes
                            [v.start as usize + offset as usize..v.start as usize + end],
                    }
                }
                Stage::Commit => Request::Commit {
                    ticket: s.ticket,
                    digest: Sha256::digest(
                        &self.bytes[v.start as usize
                            ..v.start as usize + if s.upstream { 0 } else { v.carried as usize }],
                    )
                    .into(),
                },
                Stage::Release => Request::Release { ticket: s.ticket },
            };
            let n = request.encode(out);
            return Some((index, s.target.owner, n));
        }
        None
    }
    pub fn sent(&mut self, index: usize, token: u16, now: u32) {
        if let Some(Visit {
            phase: Phase::Sending(s),
            ..
        }) = &mut self.visits[index]
        {
            s.waiting = Some(token);
            s.sent = now;
        }
    }
    pub fn awaiting(&self) -> Option<(usize, u16, u64, u32)> {
        self.visits.iter().enumerate().find_map(|(i, v)| match v {
            Some(Visit {
                phase: Phase::Sending(s),
                ..
            }) => s.waiting.map(|t| (i, t, s.target.owner, s.sent)),
            _ => None,
        })
    }
    pub fn retry(&mut self, index: usize, now: u32) {
        if matches!(
            self.visits[index],
            Some(Visit {
                phase: Phase::Sending(Sending {
                    stage: Stage::Release,
                    ..
                }),
                ..
            })
        ) {
            self.finish(index);
            return;
        }
        if let Some(Visit {
            phase: Phase::Sending(s),
            ..
        }) = &mut self.visits[index]
        {
            s.waiting = None;
            s.backoff = s.backoff.saturating_add(1).min(5);
            s.due = now.wrapping_add(50u32 << s.backoff);
        }
    }
    pub fn reply(&mut self, index: usize, reply: Reply<'_>, now: u32) {
        let Some(Visit {
            phase: Phase::Sending(mut s),
            carried,
            ..
        }) = self.visits[index]
        else {
            return;
        };
        s.waiting = None;
        if matches!(reply.status, Status::Busy | Status::Unavailable) {
            self.retry(index, now);
            return;
        }
        let result = match s.stage {
            Stage::Session if reply.status == Status::Ok && reply.bytes.len() >= 14 => {
                s.ticket.epoch = u64::from_le_bytes(reply.bytes[..8].try_into().unwrap());
                s.stage = Stage::Reserve;
                Ok(())
            }
            Stage::Reserve if matches!(reply.status, Status::Receiving | Status::Accepted) => {
                s.ticket.slot = reply.offset;
                s.ticket.generation = reply.generation;
                s.stage = if reply.status == Status::Accepted {
                    Stage::Release
                } else {
                    Stage::Data(reply.accepted)
                };
                Ok(())
            }
            Stage::Data(_) if reply.status == Status::Receiving && reply.accepted <= carried => {
                s.stage = Stage::Data(reply.accepted);
                Ok(())
            }
            Stage::Commit if reply.status == Status::Accepted => {
                s.stage = Stage::Release;
                Ok(())
            }
            Stage::Release => {
                self.finish(index);
                return;
            }
            Stage::Session | Stage::Reserve | Stage::Data(_) if reply.status == Status::Stale => {
                s.stage = Stage::Session;
                Ok(())
            }
            _ => Err(if reply.status == Status::Ok {
                Status::Invalid
            } else {
                reply.status
            }),
        };
        match result {
            Ok(()) => {
                s.backoff = 0;
                s.due = now;
                self.visits[index].as_mut().unwrap().phase = Phase::Sending(s);
                if s.stage == Stage::Release {
                    let v = self.visits[index].as_mut().unwrap();
                    v.length = 0;
                    v.carried = 0;
                }
            }
            Err(e) => self.fail(index, e),
        }
    }
}
