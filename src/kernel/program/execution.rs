//! Retained connection inputs and finite owner-local evaluations.
//! Local operation records keep their meaning. Only heads originate activation;
//! other entries resume from their current inputs, without an upstream walk.
use super::{
    evaluation::{self, Values},
    image::Definition,
    Configuration,
};
use crate::{
    ir::Value,
    kernel::{
        appliance::{execution as calls, Native},
        peer::frame::{InvokeStatus, StateReadResult},
        radio::channel::{Channel, Coverage, Latest, Received, Temporal, Unique, Update},
    },
    sync::TaskContext,
};
use lattice_peer_wire::{
    program::{
        execution::{Entry, Request, SIGNAL_BYTES},
        transfer::{Reply, Status, DATA_BYTES, REQUEST_BYTES},
        Link, Operation, Reference,
    },
    scalar,
};

const NETWORK_WORK: usize = crate::configLATTICE_EVALUATIONS;
// A local continuation can make progress even while all network work is retained.
const COUNT: usize = NETWORK_WORK + 1;
const CONNECTIONS: usize = crate::configLATTICE_CONNECTIONS;
const INPUTS: usize = CONNECTIONS * 2;
const ACTIONS: usize = CONNECTIONS * 2;
const BYTES: usize = crate::configLATTICE_PROGRAM_BYTES;
const PIECES: usize = BYTES.div_ceil(SIGNAL_BYTES);
const RECEIVING_MS: u32 = 30_000;
const _: () =
    assert!(NETWORK_WORK > 1 && CONNECTIONS > 0 && ACTIONS <= 32 && BYTES <= u16::MAX as usize);

pub(super) fn fits(d: Definition<'_>) -> Result<(), Status> {
    let mut entries = 0;
    let mut actions = 0;
    for r in d.records() {
        match r.map_err(super::status)?.operation {
            Operation::Entry { .. } => entries += 1,
            Operation::Invoke { arguments, .. } => {
                actions += 1;
                if arguments.len() > crate::configLATTICE_PROGRAM_ARGUMENTS {
                    return Err(Status::Bounds);
                }
            }
            _ => {}
        }
    }
    if entries > CONNECTIONS
        || actions > ACTIONS
        || d.carried_bytes() as usize * 2 + d.scratch_bytes() as usize > BYTES
    {
        Err(Status::Bounds)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Extent {
    start: u16,
    length: u16,
}
impl Extent {
    fn range(self) -> core::ops::Range<usize> {
        self.start as usize..self.start as usize + self.length as usize
    }
}
#[derive(Clone, Copy)]
struct Component {
    entry: Entry,
    carried: u16,
    workspace: u16,
    finished: u32,
    output: Option<Reference>,
    values: Extent,
    retiring: bool,
    writes_carried: bool,
    pending_low: Option<Publication>,
    low_blocked: bool,
}
type Level = Channel<Update<bool>, (Unique, Temporal), Latest<bool>>;
#[derive(Clone, Copy)]
struct Input {
    owner: u64,
    source: Entry,
    target: Entry,
    epoch: u64,
    level: Level,
    values: Extent,
    order: u64,
}
impl Input {
    fn active(self) -> bool {
        self.level.current().is_some_and(|(_, active)| active)
    }
}
#[derive(Clone, Copy)]
struct Action {
    root: Entry,
    record: u16,
    sequence: u32,
    active: bool,
    arguments: Extent,
    ticket: Option<calls::Ticket>,
    result: InvokeStatus,
}
#[derive(Clone, Copy)]
struct Publication {
    target: Reference,
    active: bool,
    sequence: u32,
}
#[derive(Clone, Copy)]
struct Sending {
    messages: [Option<Publication>; 2],
    at: usize,
    offset: u16,
    waiting: Option<u16>,
    sent: u32,
    due: u32,
    backoff: u8,
}
#[derive(Clone, Copy)]
enum Phase {
    Receiving {
        input: usize,
        epoch: u64,
        sequence: u32,
        coverage: Coverage<{ PIECES.div_ceil(8) }>,
        touched: u32,
    },
    Evaluate,
    Follow(Link),
    Calling {
        ticket: calls::Ticket,
        success: Link,
        failure: Link,
    },
    Sending(Sending),
}
#[derive(Clone, Copy)]
struct Evaluation {
    root: Entry,
    entry: Entry,
    sequence: u32,
    values: Extent,
    retained: Extent,
    carried: u16,
    drive: bool,
    actions: u32,
    phase: Phase,
    blocked: bool,
    order: u64,
}
#[derive(Clone, Copy, Default)]
pub(super) struct Statistics {
    pub admitted: u32,
    pub completed: u32,
    pub faults: u32,
    pub uncertain: u32, // retained wire field; no uncertain commit/replay protocol
    pub last_status: u8,
    pub last_definition: u16,
    pub last_record: u16,
}
pub(super) struct Execution {
    bytes: [u8; BYTES],
    components: [Option<Component>; CONNECTIONS],
    inputs: [Option<Input>; INPUTS],
    actions: [Option<Action>; ACTIONS],
    work: [Option<Evaluation>; COUNT],
    sequence: u32,
    order: u64,
    pub local: u64,
    pub epoch: u64,
    pub statistics: Statistics,
    recovery: u16,
    recovered: bool,
}
impl Execution {
    pub const fn new() -> Self {
        Self {
            bytes: [0; BYTES],
            components: [None; CONNECTIONS],
            inputs: [None; INPUTS],
            actions: [None; ACTIONS],
            work: [None; COUNT],
            sequence: 0,
            order: 0,
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
        }
    }
    fn next(&mut self) -> Result<u32, Status> {
        self.sequence = self.sequence.checked_add(1).ok_or(Status::Bounds)?;
        Ok(self.sequence)
    }
    fn next_order(&mut self) -> Result<u64, Status> {
        self.order = self.order.checked_add(1).ok_or(Status::Bounds)?;
        Ok(self.order)
    }
    fn extents(&self) -> impl Iterator<Item = Extent> + '_ {
        self.work
            .iter()
            .flatten()
            .flat_map(|w| [w.values, w.retained])
            .chain(self.inputs.iter().flatten().map(|i| i.values))
            .chain(self.components.iter().flatten().map(|c| c.values))
            .chain(self.actions.iter().flatten().map(|a| a.arguments))
    }
    fn extents_mut(&mut self) -> impl Iterator<Item = &mut Extent> {
        self.work
            .iter_mut()
            .flatten()
            .flat_map(|w| [&mut w.values, &mut w.retained])
            .chain(self.inputs.iter_mut().flatten().map(|i| &mut i.values))
            .chain(self.components.iter_mut().flatten().map(|c| &mut c.values))
            .chain(self.actions.iter_mut().flatten().map(|a| &mut a.arguments))
    }
    // Only arena offsets are retained; no native task borrows these bytes. Move
    // shared/overlapping extents together before admitting additional work.
    fn compact(&mut self) {
        let mut ranges = [Extent::default(); COUNT * 2 + INPUTS + CONNECTIONS + ACTIONS];
        let mut count = 0;
        for r in self.extents().filter(|r| r.length != 0) {
            ranges[count] = r;
            count += 1;
        }
        ranges[..count].sort_unstable_by_key(|r| r.start);
        let (mut i, mut destination) = (0, 0);
        while i < count {
            let start = ranges[i].start as usize;
            let mut end = ranges[i].range().end;
            i += 1;
            while i < count && ranges[i].start as usize <= end {
                end = end.max(ranges[i].range().end);
                i += 1;
            }
            if start != destination {
                self.bytes.copy_within(start..end, destination);
                for r in self.extents_mut().filter(|r| r.length != 0) {
                    if r.start as usize >= start && r.range().end <= end {
                        r.start -= (start - destination) as u16;
                    }
                }
            }
            destination += end - start;
        }
    }
    // Retained values cannot consume the space needed to replace them. Account
    // declared capacities, including every bounded concurrent work slot, before
    // accepting a new connection/input. Actual extents still use only their bytes.
    fn capacity(&self, additional: usize, workspace: usize) -> Result<(), Status> {
        let outputs: usize = self
            .components
            .iter()
            .flatten()
            .map(|c| c.carried as usize)
            .sum();
        let inputs: usize = self
            .inputs
            .iter()
            .flatten()
            .map(|i| {
                self.components
                    .iter()
                    .flatten()
                    .find(|c| c.entry == i.target)
                    .map_or(0, |c| c.carried as usize)
            })
            .sum();
        let arguments: usize = self
            .actions
            .iter()
            .flatten()
            .map(|a| a.arguments.length as usize)
            .sum();
        let largest = self
            .components
            .iter()
            .flatten()
            .map(|c| c.workspace as usize)
            .max()
            .unwrap_or(0)
            .max(workspace);
        if outputs + inputs + arguments + additional + COUNT * largest > BYTES {
            Err(Status::Bounds)
        } else {
            Ok(())
        }
    }
    fn extent(&self, length: usize) -> Result<Extent, Status> {
        if length == 0 {
            return Ok(Extent::default());
        }
        let mut at = 0usize;
        loop {
            if at + length > BYTES {
                return Err(Status::Busy);
            }
            match self
                .extents()
                .filter(|r| r.length != 0 && at < r.range().end && at + length > r.range().start)
                .map(|r| r.range().end)
                .max()
            {
                Some(end) => at = end,
                None => {
                    return Ok(Extent {
                        start: at as u16,
                        length: length as u16,
                    })
                }
            }
        }
    }
    fn component(&self, entry: Entry) -> Option<usize> {
        self.components
            .iter()
            .position(|c| c.is_some_and(|c| c.entry == entry && !c.retiring))
    }
    fn ensure(&mut self, programs: &Configuration, entry: Entry) -> Result<usize, Status> {
        if let Some(i) = self.component(entry) {
            return Ok(i);
        }
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
        let slot = self
            .components
            .iter()
            .position(Option::is_none)
            .ok_or(Status::Bounds)?;
        let mut offsets = [0u16; super::validation::MAX_RECORDS];
        let mut reachable = [false; super::validation::MAX_RECORDS];
        let mut count = 0;
        for r in d.records() {
            let r = r.map_err(super::status)?;
            offsets[count] = r.offset;
            reachable[count] = r.offset == entry.record;
            count += 1;
        }
        let mut invoked = [false; super::validation::MAX_RECORDS];
        let mut needed = 0;
        let mut argument_bytes = 0;
        let mut writes_carried = false;
        for i in 0..count {
            if !reachable[i] {
                continue;
            }
            let op = d.record(offsets[i]).map_err(super::status)?.operation;
            if matches!(op, Operation::Read { output, .. } | Operation::Unary { output, .. }
                | Operation::Binary { output, .. } if output.region == lattice_peer_wire::program::Region::Carried)
            {
                writes_carried = true;
            }
            if let Operation::Invoke { arguments, .. } = op {
                invoked[i] = true;
                needed += 1;
                argument_bytes += arguments
                    .iter()
                    .map(|a| lattice_peer_wire::program::width(a.ty()) as usize)
                    .sum::<usize>();
            }
            for link in op.successors() {
                if let Link::Local(target) = link {
                    let j = offsets[..count]
                        .iter()
                        .position(|&at| at == target)
                        .ok_or(Status::Binding)?;
                    reachable[j] = true;
                }
            }
        }
        if self.actions.iter().filter(|a| a.is_none()).count() < needed {
            return Err(Status::Bounds);
        }
        let workspace = d.carried_bytes() as usize * (1 + usize::from(writes_carried))
            + d.scratch_bytes() as usize;
        self.capacity(d.carried_bytes() as usize + argument_bytes, workspace)?;
        for i in 0..count {
            if invoked[i] {
                let at = self.actions.iter().position(Option::is_none).unwrap();
                let Operation::Invoke { arguments, .. } =
                    d.record(offsets[i]).map_err(super::status)?.operation
                else {
                    unreachable!()
                };
                let length = arguments
                    .iter()
                    .map(|a| lattice_peer_wire::program::width(a.ty()) as usize)
                    .sum();
                let argument_space = match self.extent(length) {
                    Ok(v) => v,
                    Err(error) => {
                        for action in &mut self.actions {
                            if action.is_some_and(|a| a.root == entry) {
                                *action = None;
                            }
                        }
                        return Err(error);
                    }
                };
                self.actions[at] = Some(Action {
                    root: entry,
                    record: offsets[i],
                    sequence: 0,
                    active: false,
                    arguments: argument_space,
                    ticket: None,
                    result: InvokeStatus::Invoked,
                });
            }
        }
        self.components[slot] = Some(Component {
            entry,
            carried: d.carried_bytes(),
            workspace: workspace as u16,
            finished: 0,
            output: None,
            values: Extent::default(),
            retiring: false,
            writes_carried,
            pending_low: None,
            low_blocked: false,
        });
        Ok(slot)
    }
    fn selected(&self, entry: Entry, exclude: Option<usize>) -> Option<usize> {
        self.inputs
            .iter()
            .enumerate()
            .filter(|(i, v)| {
                Some(*i) != exclude && v.is_some_and(|v| v.target == entry && v.active())
            })
            .max_by_key(|(_, v)| v.unwrap().order)
            .map(|(i, _)| i)
    }
    fn vacant_work(&self, local: bool) -> Result<usize, Status> {
        if !local && self.work.iter().flatten().count() >= NETWORK_WORK - 1 {
            return Err(Status::Busy);
        }
        self.work[..if local { COUNT } else { NETWORK_WORK }]
            .iter()
            .position(Option::is_none)
            .ok_or(Status::Busy)
    }
    fn free_span(&self, start: usize, end: usize) -> bool {
        end <= BYTES
            && self
                .extents()
                .all(|r| r.length == 0 || end <= r.range().start || start >= r.range().end)
    }
    fn reserve_work_at(
        &mut self,
        programs: &Configuration,
        entry: Entry,
        drive: bool,
        source: Option<Extent>,
        slot: usize,
    ) -> Result<usize, Status> {
        let d = programs
            .part(entry.definition, entry.generation)?
            .definition;
        let component = self.ensure(programs, entry)?;
        let carried = if drive { d.carried_bytes() as usize } else { 0 };
        let length = if drive {
            carried + d.scratch_bytes() as usize
        } else {
            0
        };
        if source.is_some_and(|s| s.length as usize != carried) {
            return Err(Status::Bounds);
        }
        let alias = source.filter(|source| {
            !self.components[component].unwrap().writes_carried
                && self.free_span(
                    source.start as usize + carried,
                    source.start as usize + length,
                )
        });
        let values = if let Some(source) = alias {
            Extent {
                start: source.start,
                length: length as u16,
            }
        } else {
            self.extent(length)?
        };
        let sequence = self.next()?;
        let order = self.next_order()?;
        if alias.is_none() {
            self.bytes[values.range()].fill(0);
            if let Some(from) = source {
                self.bytes.copy_within(from.range(), values.start as usize);
            }
        } else {
            self.bytes[values.start as usize + carried..values.range().end].fill(0);
        }
        self.work[slot] = Some(Evaluation {
            root: entry,
            entry,
            sequence,
            values,
            retained: Extent::default(),
            carried: carried as u16,
            drive,
            actions: 0,
            phase: Phase::Evaluate,
            blocked: false,
            order,
        });
        Ok(slot)
    }
    fn close(&mut self, entry: Entry) -> Result<(), Status> {
        let at = self.component(entry).ok_or(Status::Stale)?;
        let sequence = self.next()?;
        let c = self.components[at].as_mut().unwrap();
        c.finished = sequence;
        if let Some(target) = c.output.take() {
            debug_assert!(c.pending_low.is_none());
            c.pending_low = Some(Publication {
                target,
                active: false,
                sequence,
            });
            c.low_blocked = false;
        }
        c.values = Extent::default();
        for a in self
            .actions
            .iter_mut()
            .flatten()
            .filter(|a| a.root == entry)
        {
            a.active = false;
            a.sequence = sequence;
        }
        Ok(())
    }
    pub fn activate(&mut self, programs: &Configuration, entry: Entry) -> Result<(), Status> {
        if self.epoch == 0 {
            return Err(Status::Busy);
        }
        self.compact();
        let d = programs
            .part(entry.definition, entry.generation)?
            .definition;
        self.ensure(programs, entry)?;
        let input = self.selected(entry, None);
        let drive = if d.head() == Some(entry.record) {
            programs.enabled(entry.definition)
        } else {
            input.is_some()
        };
        if !drive {
            self.close(entry)?;
            self.statistics.completed = self.statistics.completed.wrapping_add(1);
        } else {
            let slot = self.vacant_work(false)?;
            let source = input.map(|i| self.inputs[i].unwrap().values);
            self.reserve_work_at(programs, entry, true, source, slot)?;
        }
        self.statistics.admitted = self.statistics.admitted.wrapping_add(1);
        Ok(())
    }
    pub fn wake(&mut self) {
        for work in self.work.iter_mut().flatten() {
            work.blocked = false;
        }
        for c in self.components.iter_mut().flatten() {
            c.low_blocked = false;
        }
    }
    fn release(&mut self, index: usize) {
        self.work[index] = None;
        self.statistics.completed = self.statistics.completed.wrapping_add(1);
        self.wake();
    }
    pub fn fault(&mut self, entry: Entry, status: Status) {
        self.statistics.faults = self.statistics.faults.wrapping_add(1);
        self.statistics.last_status = status as u8;
        self.statistics.last_definition = entry.definition;
        self.statistics.last_record = entry.record;
    }
    fn fail(&mut self, index: usize, status: Status) {
        if let Some(work) = self.work[index] {
            if let Phase::Calling { ticket, .. } = work.phase {
                calls::detach(ticket);
            }
            self.fault(work.entry, status);
            self.release(index);
        }
    }
    pub fn remove(&mut self, definition: u16, generation: u32) {
        let matches = |e: Entry| e.definition == definition && e.generation == generation;
        for i in 0..COUNT {
            let Some(work) = self.work[i].filter(|w| matches(w.root)) else {
                continue;
            };
            if let Phase::Calling { ticket, .. } = work.phase {
                calls::detach(ticket);
            }
            if let Phase::Sending(mut s) = work.phase {
                for p in &mut s.messages {
                    if p.is_some_and(|p| p.active) {
                        *p = None;
                    }
                }
                if let Some(at) = (s.at..2).find(|&i| s.messages[i].is_some()) {
                    if at != s.at {
                        s.waiting = None;
                    }
                    s.at = at;
                    s.offset = 0;
                    let w = self.work[i].as_mut().unwrap();
                    w.phase = Phase::Sending(s);
                    w.values = Extent::default();
                    w.carried = 0;
                    continue;
                }
            }
            self.release(i);
        }
        for i in 0..CONNECTIONS {
            if let Some(c) = self.components[i].filter(|c| matches(c.entry)) {
                if let Err(error) = self.close(c.entry) {
                    self.fault(c.entry, error);
                }
                self.components[i].as_mut().unwrap().retiring = true;
            }
        }
        for input in &mut self.inputs {
            if input.is_some_and(|i| matches(i.target)) {
                *input = None;
            }
        }
        for action in &mut self.actions {
            if action.is_some_and(|a| matches(a.root)) {
                *action = None;
            }
        }
    }
    fn retire(&mut self) {
        for i in 0..CONNECTIONS {
            let Some(c) = self.components[i] else {
                continue;
            };
            if let Some(p) = c.pending_low.filter(|p| p.target.owner != self.local) {
                let Ok(slot) = self.vacant_work(false) else {
                    continue;
                };
                let Ok(order) = self.next_order() else {
                    continue;
                };
                self.work[slot] = Some(Evaluation {
                    root: c.entry,
                    entry: c.entry,
                    sequence: p.sequence,
                    values: Extent::default(),
                    retained: Extent::default(),
                    carried: 0,
                    drive: false,
                    actions: 0,
                    phase: Phase::Sending(Sending {
                        messages: [Some(p), None],
                        at: 0,
                        offset: 0,
                        waiting: None,
                        sent: 0,
                        due: 0,
                        backoff: 0,
                    }),
                    blocked: false,
                    order,
                });
                self.components[i].as_mut().unwrap().pending_low = None;
            }
            if self.components[i].is_some_and(|c| c.retiring && c.pending_low.is_none()) {
                self.components[i] = None;
            }
        }
    }
    fn finish(&mut self, index: usize, target: Option<Reference>) -> Result<(), Status> {
        let work = self.work[index].ok_or(Status::Stale)?;
        let Some(at) = self.component(work.root) else {
            self.release(index);
            return Ok(());
        };
        let old = self.components[at].unwrap();
        if work.sequence < old.finished {
            self.release(index);
            return Ok(());
        }
        let target = target.filter(|_| work.drive);
        let output = if target.is_some() {
            Extent {
                start: work.values.start,
                length: work.carried,
            }
        } else {
            Extent::default()
        };
        let equal = old.output == target
            && (target.is_none() || self.bytes[old.values.range()] == self.bytes[output.range()]);
        debug_assert!(old.pending_low.is_none() || old.output.is_none());
        let messages = [
            target.filter(|_| !equal).map(|target| Publication {
                target,
                active: true,
                sequence: work.sequence,
            }),
            old.pending_low.or_else(|| {
                old.output
                    .filter(|previous| Some(*previous) != target)
                    .map(|target| Publication {
                        target,
                        active: false,
                        sequence: work.sequence,
                    })
            }),
        ];
        for (i, action) in self.actions.iter_mut().enumerate() {
            if let Some(a) = action.as_mut().filter(|a| {
                a.root == work.root && a.sequence <= work.sequence && work.actions & (1 << i) == 0
            }) {
                a.active = false;
                a.sequence = work.sequence;
            }
        }
        self.components[at] = Some(Component {
            finished: work.sequence,
            output: target,
            values: output,
            pending_low: None,
            low_blocked: false,
            ..old
        });
        if messages.iter().all(Option::is_none) {
            self.release(index);
        } else {
            let w = self.work[index].as_mut().unwrap();
            w.retained = Extent::default();
            if target.is_none() {
                w.values = Extent::default();
                w.carried = 0;
            } else {
                w.values.length = w.carried;
            }
            w.phase = Phase::Sending(Sending {
                messages,
                at: usize::from(messages[0].is_none()),
                offset: 0,
                waiting: None,
                sent: 0,
                due: 0,
                backoff: 0,
            });
            w.blocked = false;
        }
        Ok(())
    }
    fn follow(&mut self, index: usize, link: Link) -> Result<bool, Status> {
        match link {
            Link::Local(record) => {
                let w = self.work[index].as_mut().ok_or(Status::Stale)?;
                w.entry.record = record;
                w.phase = Phase::Evaluate;
                Ok(true)
            }
            Link::Stop => {
                self.finish(index, None)?;
                Ok(false)
            }
            Link::Remote(target) => {
                self.finish(index, Some(target))?;
                Ok(false)
            }
        }
    }
    fn arguments_equal(&self, saved: Extent, args: &[Value]) -> bool {
        let mut at = saved.start as usize;
        for value in args {
            let mut encoded = [0; 9];
            let n = scalar::encode_value(*value, &mut encoded) - 1;
            if at + n > saved.range().end || self.bytes[at..at + n] != encoded[1..1 + n] {
                return false;
            }
            at += n;
        }
        at == saved.range().end
    }
    fn invoke(
        &mut self,
        index: usize,
        native: &'static dyn Native,
        context: &TaskContext,
        transformer: u64,
        args: &[Value],
        success: Link,
        failure: Link,
    ) -> Result<bool, Status> {
        let w = self.work[index].unwrap();
        let at = self
            .actions
            .iter()
            .position(|a| a.is_some_and(|a| a.root == w.root && a.record == w.entry.record))
            .ok_or(Status::Binding)?;
        let mut a = self.actions[at].unwrap();
        if a.active && self.arguments_equal(a.arguments, args) {
            self.work[index].as_mut().unwrap().actions |= 1 << at;
            a.sequence = a.sequence.max(w.sequence);
            self.actions[at] = Some(a);
            self.work[index].as_mut().unwrap().phase = match a.ticket {
                Some(ticket) => Phase::Calling {
                    ticket,
                    success,
                    failure,
                },
                None => Phase::Follow(if a.result == InvokeStatus::Invoked {
                    success
                } else {
                    failure
                }),
            };
            return Ok(a.ticket.is_none());
        }
        let length: usize = args
            .iter()
            .map(|v| lattice_peer_wire::program::width(v.ty()) as usize)
            .sum();
        if a.arguments.length as usize != length {
            return Err(Status::Binding);
        }
        let saved = a.arguments;
        let ticket = calls::submit_values(native, context, transformer, args).map_err(|e| {
            if e == InvokeStatus::Busy {
                Status::Busy
            } else {
                Status::Binding
            }
        })?;
        if w.sequence >= a.sequence {
            let mut cursor = saved.start as usize;
            for value in args {
                let mut encoded = [0; 9];
                let n = scalar::encode_value(*value, &mut encoded) - 1;
                self.bytes[cursor..cursor + n].copy_from_slice(&encoded[1..1 + n]);
                cursor += n;
            }
            a.active = true;
            a.sequence = w.sequence;
            a.arguments = saved;
            a.ticket = Some(ticket);
            a.result = InvokeStatus::Busy;
            self.actions[at] = Some(a);
        }
        let w = self.work[index].as_mut().unwrap();
        w.actions |= 1 << at;
        w.phase = Phase::Calling {
            ticket,
            success,
            failure,
        };
        Ok(false)
    }

    fn step(
        &mut self,
        index: usize,
        v: Evaluation,
        programs: &Configuration,
        native: &'static dyn Native,
        context: &TaskContext,
    ) -> Result<bool, Status> {
        if let Phase::Follow(link) = v.phase {
            return self.follow(index, link);
        }
        if !matches!(v.phase, Phase::Evaluate) {
            return Ok(false);
        }
        if !v.drive {
            self.finish(index, None)?;
            return Ok(false);
        }
        let d = programs
            .part(v.entry.definition, v.entry.generation)?
            .definition;
        let op = d.record(v.entry.record).map_err(super::status)?.operation;
        let (carried, scratch) = self.bytes
            [v.values.start as usize..v.values.start as usize + v.values.length as usize]
            .split_at_mut(v.carried as usize);
        let mut values = Values { carried, scratch };
        let next = match op {
            Operation::Temporal { yes, no, .. } => {
                let source = v.entry.definition
                    + super::PREFIX as u16
                    + d.storage_record(v.entry.record).ok_or(Status::Binding)?;
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
                return self.invoke(
                    index,
                    native,
                    context,
                    transformer,
                    &args[..arguments.len()],
                    success,
                    failure,
                );
            }
        };
        self.work[index].as_mut().unwrap().phase = Phase::Follow(next);
        self.follow(index, next)
    }

    pub fn dispatch(
        &mut self,
        programs: &Configuration,
        native: &'static dyn Native,
        context: &TaskContext,
    ) {
        for _ in 0..COUNT {
            for i in 0..COUNT {
                let Some(Evaluation {
                    phase: Phase::Calling { ticket, .. },
                    ..
                }) = self.work[i]
                else {
                    continue;
                };
                let Some(result) = calls::take_result(ticket) else {
                    continue;
                };
                for a in self
                    .actions
                    .iter_mut()
                    .flatten()
                    .filter(|a| a.ticket == Some(ticket))
                {
                    a.ticket = None;
                    a.result = result;
                }
                for w in self.work.iter_mut().flatten() {
                    if let Phase::Calling {
                        ticket: waiting,
                        success,
                        failure,
                    } = w.phase
                    {
                        if waiting == ticket {
                            w.phase = Phase::Follow(if result == InvokeStatus::Invoked {
                                success
                            } else {
                                failure
                            });
                            w.blocked = false;
                        }
                    }
                }
            }
            let ready = self
                .work
                .iter()
                .enumerate()
                .filter_map(|(i, w)| {
                    w.filter(|w| {
                        !w.blocked && matches!(w.phase, Phase::Evaluate | Phase::Follow(_))
                    })
                    .map(|w| (i, w.order))
                })
                .min_by_key(|(_, order)| *order)
                .map(|(i, _)| i);
            let Some(index) = ready else {
                break;
            };
            for _ in 0..super::validation::MAX_RECORDS {
                let Some(w) = self.work[index] else {
                    break;
                };
                match self.step(index, w, programs, native, context) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(Status::Busy | Status::Unavailable) => {
                        self.work[index].as_mut().unwrap().blocked = true;
                        break;
                    }
                    Err(error) => {
                        self.fail(index, error);
                        break;
                    }
                }
            }
        }
    }
    pub fn pending(&self) -> bool {
        self.work.iter().any(Option::is_some)
            || self
                .components
                .iter()
                .flatten()
                .any(|c| c.retiring || c.pending_low.is_some())
            || (!self.recovered && self.epoch != 0)
    }
    pub fn wait_ms(&self, now: u32, can_send: bool) -> Option<u32> {
        let mut next = None;
        let mut include = |ms| next = Some(next.map_or(ms, |old: u32| old.min(ms)));
        for w in self.work.iter().flatten() {
            match w.phase {
                Phase::Evaluate | Phase::Follow(_) if !w.blocked => include(0),
                Phase::Receiving { touched, .. } => {
                    include(RECEIVING_MS.saturating_sub(now.wrapping_sub(touched)))
                }
                Phase::Sending(s) if s.waiting.is_some() => {
                    include(2000u32.saturating_sub(now.wrapping_sub(s.sent)))
                }
                Phase::Sending(s)
                    if s.messages[s.at].is_some_and(|p| p.target.owner == self.local)
                        && !w.blocked =>
                {
                    include(0)
                }
                Phase::Sending(s) if can_send && self.awaiting().is_none() && !w.blocked => {
                    include(if s.backoff == 0 || now.wrapping_sub(s.due) < 1 << 31 {
                        0
                    } else {
                        s.due.wrapping_sub(now)
                    })
                }
                _ => {}
            }
        }
        if self.components.iter().flatten().any(|c| {
            c.pending_low.is_some_and(|p| {
                (p.target.owner == self.local && !c.low_blocked)
                    || (p.target.owner != self.local && self.vacant_work(false).is_ok())
            })
        }) {
            include(0);
        }
        next
    }
    pub fn poll(
        &mut self,
        programs: &mut Configuration,
        native: &'static dyn Native,
        context: &TaskContext,
        now: u32,
    ) -> bool {
        if self.local == 0 || self.epoch == 0 {
            return false;
        }
        for i in 0..COUNT {
            if self.work[i].is_some_and(|w| {
                matches!(w.phase, Phase::Receiving { touched, .. }
                if now.wrapping_sub(touched) >= RECEIVING_MS)
            }) {
                self.work[i] = None;
                self.wake();
            }
        }
        self.clean_inputs();
        self.retire();
        let mut again = false;
        for _ in 0..CONNECTIONS {
            let pending = self.components.iter().enumerate().find_map(|(i, c)| {
                let c = (*c)?;
                c.pending_low
                    .filter(|p| p.target.owner == self.local && !c.low_blocked)
                    .map(|p| (i, c, p))
            });
            let Some((i, c, p)) = pending else {
                break;
            };
            let target = Entry {
                definition: p.target.definition,
                generation: p.target.generation,
                record: p.target.record,
            };
            let result = self.signal(
                programs,
                self.local,
                c.entry,
                target,
                self.epoch,
                p.sequence,
                false,
                0,
                &[],
                now,
            );
            if result == Err(Status::Busy) {
                self.components[i].as_mut().unwrap().low_blocked = true;
            } else {
                if let Err(error) = result {
                    if error != Status::Stale {
                        self.fault(c.entry, error);
                    }
                }
                if let Some(c) = self.components[i].as_mut() {
                    c.pending_low = None;
                }
                again = true;
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
                        programs.bytes[entry.definition as usize + 2] &= !2;
                        again = true;
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
                            again = true;
                        }
                    },
                    None => self.recovered = true,
                }
            }
        }
        self.dispatch(programs, native, context);
        // A local continuation uses the same acceptance path, without occupying
        // a radio request or waiting on a peer/time-reference exchange.
        for _ in 0..COUNT {
            let mut bytes = [0; REQUEST_BYTES];
            let Some((index, owner, length)) = self.outgoing_matching(now, &mut bytes, true) else {
                break;
            };
            debug_assert_eq!(owner, self.local);
            let request = Request::decode(&bytes[..length]).unwrap();
            let mut data = [0; DATA_BYTES];
            let reply = self.receive(programs, self.local, request, now, &mut data);
            if reply.status == Status::Busy {
                self.work[index].as_mut().unwrap().blocked = true;
            } else {
                self.reply(index, reply, now);
                again = true;
            }
        }
        again
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
        match request {
            Request::Session => {
                out[..8].copy_from_slice(&self.epoch.to_le_bytes());
                for (i, n) in [
                    BYTES,
                    COUNT,
                    crate::configLATTICE_PROGRAM_ARGUMENTS,
                    CONNECTIONS,
                    INPUTS,
                    ACTIONS,
                ]
                .into_iter()
                .enumerate()
                {
                    out[8 + i * 2..10 + i * 2].copy_from_slice(&(n as u16).to_le_bytes());
                }
                Reply {
                    bytes: &out[..20],
                    ..Reply::status(Status::Ok)
                }
            }
            Request::Statistics => {
                let s = self.statistics;
                for (i, n) in [
                    s.admitted,
                    s.completed,
                    s.faults,
                    s.uncertain,
                    super::activation::overflow(),
                ]
                .into_iter()
                .enumerate()
                {
                    out[i * 4..i * 4 + 4].copy_from_slice(&n.to_le_bytes());
                }
                out[20] = s.last_status;
                out[21..23].copy_from_slice(&s.last_definition.to_le_bytes());
                out[23..25].copy_from_slice(&s.last_record.to_le_bytes());
                Reply {
                    bytes: &out[..25],
                    ..Reply::status(Status::Ok)
                }
            }
            Request::Signal {
                source,
                target,
                epoch,
                sequence,
                active,
                offset,
                bytes,
            } => {
                match self.signal(
                    programs, caller, source, target, epoch, sequence, active, offset, bytes, now,
                ) {
                    Ok((accepted, total, done)) => Reply {
                        accepted,
                        total,
                        ..Reply::status(if done {
                            Status::Accepted
                        } else {
                            Status::Receiving
                        })
                    },
                    Err(error) => Reply::status(error),
                }
            }
            _ => Reply::status(Status::Unsupported),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn signal(
        &mut self,
        programs: &Configuration,
        caller: u64,
        source: Entry,
        target: Entry,
        epoch: u64,
        sequence: u32,
        active: bool,
        offset: u16,
        bytes: &[u8],
        now: u32,
    ) -> Result<(u16, u16, bool), Status> {
        if caller == 0
            || caller == u64::MAX
            || epoch == 0
            || sequence == 0
            || source.generation == 0
        {
            return Err(Status::Invalid);
        }
        let d = programs
            .part(target.definition, target.generation)?
            .definition;
        if d.head() == Some(target.record) {
            return Err(Status::Binding);
        }
        let Operation::Entry { inputs, .. } =
            d.record(target.record).map_err(super::status)?.operation
        else {
            return Err(Status::Binding);
        };
        fits(d)?;
        let total = if active { d.carried_bytes() } else { 0 };
        let start = offset as usize;
        let end = start.checked_add(bytes.len()).ok_or(Status::Bounds)?;
        if end > total as usize
            || bytes.len() > SIGNAL_BYTES
            || (total == 0 && (offset != 0 || !bytes.is_empty()))
            || (total != 0
                && (start >= total as usize
                    || start % SIGNAL_BYTES != 0
                    || bytes.len() != SIGNAL_BYTES.min(total as usize - start)))
        {
            return Err(Status::Bounds);
        }
        self.compact();
        self.ensure(programs, target)?;
        let existing = self.inputs.iter().position(|i| {
            i.is_some_and(|i| i.owner == caller && i.source == source && i.target == target)
        });
        let input = match existing {
            Some(i) => i,
            None => self
                .inputs
                .iter()
                .position(Option::is_none)
                .ok_or(Status::Bounds)?,
        };
        if existing.is_none() {
            self.capacity(d.carried_bytes() as usize, 0)?;
        }
        if let Some(i) = self.inputs[input] {
            if epoch < i.epoch
                || (epoch == i.epoch
                    && i.level.inspect(&(sequence, active)) != Ok(Received::Accepted))
            {
                return Ok((total, total, true));
            }
        }

        if !active {
            let selected = self.selected(target, Some(input));
            let order = if let Some(selected) = selected {
                let slot = self.vacant_work(true)?;
                let from = self.inputs[selected].unwrap().values;
                let work = self.reserve_work_at(programs, target, true, Some(from), slot)?;
                self.work[work].unwrap().order
            } else {
                let order = self.next_order()?;
                self.close(target)?;
                self.statistics.completed = self.statistics.completed.wrapping_add(1);
                order
            };
            let mut level = if self.inputs[input].is_some_and(|i| i.epoch == epoch) {
                self.inputs[input].unwrap().level
            } else {
                Channel::new(Latest::new())
            };
            level
                .receive(&(sequence, false))
                .map_err(|_| Status::Invalid)?;
            self.inputs[input] = Some(Input {
                owner: caller,
                source,
                target,
                epoch,
                level,
                values: Extent::default(),
                order,
            });
            self.discard_partial(input, epoch, sequence);
            self.statistics.admitted = self.statistics.admitted.wrapping_add(1);
            return Ok((0, 0, true));
        }

        let pending = self.work.iter().position(|w| {
            w.is_some_and(|w| {
                matches!(w.phase,
            Phase::Receiving { input: i, epoch: e, sequence: s, .. }
                if i == input && e == epoch && s == sequence)
            })
        });
        let work = if let Some(i) = pending {
            i
        } else {
            let slot = self.vacant_work(true)?;
            let i = self.reserve_work_at(programs, target, true, None, slot)?;
            if self.components[self.component(target).unwrap()]
                .unwrap()
                .writes_carried
            {
                match self.extent(total as usize) {
                    Ok(retained) => self.work[i].as_mut().unwrap().retained = retained,
                    Err(error) => {
                        self.work[i] = None;
                        return Err(error);
                    }
                }
            }
            if existing.is_none() {
                self.inputs[input] = Some(Input {
                    owner: caller,
                    source,
                    target,
                    epoch: 0,
                    level: Channel::new(Latest::new()),
                    values: Extent::default(),
                    order: 0,
                });
            }
            self.work[i].as_mut().unwrap().phase = Phase::Receiving {
                input,
                epoch,
                sequence,
                coverage: Coverage::new(),
                touched: now,
            };
            i
        };
        let w = self.work[work].unwrap();
        let Phase::Receiving { mut coverage, .. } = w.phase else {
            unreachable!()
        };
        let piece = start / SIGNAL_BYTES;
        if total != 0 && !coverage.contains(piece, piece + 1) {
            self.bytes[w.values.start as usize + start..w.values.start as usize + end]
                .copy_from_slice(bytes);
            coverage
                .mark(piece, piece + 1)
                .map_err(|_| Status::Bounds)?;
        }
        self.work[work].as_mut().unwrap().phase = Phase::Receiving {
            input,
            epoch,
            sequence,
            coverage,
            touched: now,
        };
        let pieces = (total as usize).div_ceil(SIGNAL_BYTES);
        let accepted = (coverage.prefix(pieces) * SIGNAL_BYTES).min(total as usize) as u16;
        if accepted != total {
            return Ok((accepted, total, false));
        }
        let valid = (|| {
            let (carried, scratch) = self.bytes[w.values.range()].split_at_mut(w.carried as usize);
            let values = Values { carried, scratch };
            for operand in inputs.iter() {
                values.read(operand).map_err(super::status)?;
            }
            Ok::<_, Status>(())
        })();
        if let Err(error) = valid {
            self.work[work] = None;
            self.clean_inputs();
            self.wake();
            return Err(error);
        }
        let old = self.inputs[input].unwrap();
        let saved = if self.components[self.component(target).unwrap()]
            .unwrap()
            .writes_carried
        {
            self.bytes.copy_within(
                w.values.start as usize..w.values.start as usize + total as usize,
                w.retained.start as usize,
            );
            w.retained
        } else {
            Extent {
                start: w.values.start,
                length: total,
            }
        };
        let mut level = if old.epoch == epoch {
            old.level
        } else {
            Channel::new(Latest::new())
        };
        level
            .receive(&(sequence, true))
            .map_err(|_| Status::Invalid)?;
        let order = self.next_order()?;
        let version = self.next()?;
        self.inputs[input] = Some(Input {
            epoch,
            level,
            values: saved,
            order,
            ..old
        });
        let ready = self.work[work].as_mut().unwrap();
        ready.sequence = version;
        ready.order = order;
        ready.phase = Phase::Evaluate;
        ready.retained = Extent::default();
        ready.blocked = false;
        self.discard_partial(input, epoch, sequence);
        self.statistics.admitted = self.statistics.admitted.wrapping_add(1);
        Ok((total, total, true))
    }
    fn discard_partial(&mut self, input: usize, epoch: u64, sequence: u32) {
        for w in &mut self.work {
            if w.is_some_and(|w| {
                matches!(w.phase,Phase::Receiving { input:i, epoch:e, sequence:s, .. }
                if i == input && (e < epoch || (e == epoch && (s == sequence || crate::kernel::radio::channel::newer(sequence,s)))))
            }) {
                *w = None;
            }
        }
        self.wake();
    }
    fn clean_inputs(&mut self) {
        for i in 0..INPUTS {
            if self.inputs[i].is_some_and(|input| input.level.current().is_none())
                && !self
                    .work
                    .iter()
                    .flatten()
                    .any(|w| matches!(w.phase,Phase::Receiving {input,..} if input == i))
            {
                self.inputs[i] = None;
            }
        }
    }
    pub fn outgoing(
        &mut self,
        now: u32,
        out: &mut [u8; REQUEST_BYTES],
    ) -> Option<(usize, u64, usize)> {
        self.outgoing_matching(now, out, false)
    }
    fn outgoing_matching(
        &mut self,
        now: u32,
        out: &mut [u8; REQUEST_BYTES],
        local: bool,
    ) -> Option<(usize, u64, usize)> {
        if !local && self.awaiting().is_some() {
            return None;
        }
        let chosen = self
            .work
            .iter()
            .enumerate()
            .filter_map(|(index, w)| {
                let w = (*w)?;
                let Phase::Sending(s) = w.phase else {
                    return None;
                };
                let publication = s.messages[s.at]?;
                (!w.blocked
                    && s.waiting.is_none()
                    && (publication.target.owner == self.local) == local
                    && (s.backoff == 0 || now.wrapping_sub(s.due) < 1 << 31))
                    .then_some((index, w, s, publication))
            })
            .min_by_key(|(_, w, _, _)| w.order);
        let (index, w, s, p) = chosen?;
        let length = if p.active { w.carried as usize } else { 0 };
        let end = (s.offset as usize + SIGNAL_BYTES).min(length);
        let bytes =
            &self.bytes[w.values.start as usize + s.offset as usize..w.values.start as usize + end];
        let target = Entry {
            definition: p.target.definition,
            generation: p.target.generation,
            record: p.target.record,
        };
        let n = Request::Signal {
            source: w.root,
            target,
            epoch: self.epoch,
            sequence: p.sequence,
            active: p.active,
            offset: s.offset,
            bytes,
        }
        .encode(out);
        Some((index, p.target.owner, n))
    }
    pub fn sent(&mut self, index: usize, token: u16, now: u32) {
        if let Some(w) = self.work[index].as_mut() {
            if let Phase::Sending(s) = &mut w.phase {
                s.waiting = Some(token);
                s.sent = now;
            }
        }
    }
    pub fn awaiting(&self) -> Option<(usize, u16, u64, u32)> {
        self.work.iter().enumerate().find_map(|(i, w)| {
            let Phase::Sending(s) = w.as_ref()?.phase else {
                return None;
            };
            s.waiting
                .map(|token| (i, token, s.messages[s.at].unwrap().target.owner, s.sent))
        })
    }
    pub fn retry(&mut self, index: usize, now: u32) {
        if let Some(Evaluation {
            phase: Phase::Sending(s),
            ..
        }) = self.work[index].as_mut()
        {
            s.waiting = None;
            s.backoff = s.backoff.saturating_add(1).min(5);
            s.due = now.wrapping_add(50u32 << s.backoff);
        }
    }
    fn next_publication(&mut self, index: usize) {
        let Some(w) = self.work[index].as_mut() else {
            return;
        };
        let Phase::Sending(s) = &mut w.phase else {
            return;
        };
        s.at += 1;
        if s.at == 2 || s.messages[s.at].is_none() {
            self.release(index);
        } else {
            s.offset = 0;
            s.waiting = None;
            s.backoff = 0;
            s.due = 0;
            if !s.messages[s.at].unwrap().active {
                w.values = Extent::default();
                w.carried = 0;
            }
        }
    }
    pub fn reply(&mut self, index: usize, reply: Reply<'_>, now: u32) {
        let Some(w) = self.work[index] else {
            return;
        };
        let Phase::Sending(mut s) = w.phase else {
            return;
        };
        if reply.status == Status::Busy {
            self.retry(index, now);
            return;
        }
        let p = s.messages[s.at].unwrap();
        let total = if p.active { w.carried } else { 0 };
        if !matches!(reply.status, Status::Accepted | Status::Receiving) {
            if !p.active && reply.status == Status::Stale {
                self.next_publication(index);
            } else {
                self.fail(index, reply.status);
            }
            return;
        }
        if reply.total != total || reply.accepted > total {
            self.fail(index, Status::Bounds);
            return;
        }
        match reply.status {
            Status::Accepted if reply.accepted == total => self.next_publication(index),
            Status::Receiving if reply.accepted < total => {
                s.offset = reply.accepted;
                s.waiting = None;
                s.backoff = 0;
                s.due = now;
                self.work[index].as_mut().unwrap().phase = Phase::Sending(s);
            }
            status => self.fail(index, status),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_compaction() {
        let mut e = Execution::new();
        for (i, byte) in e.bytes.iter_mut().enumerate() {
            *byte = i as u8;
        }
        for (i, (start, length)) in [(6, 4), (8, 2), (18, 3)].into_iter().enumerate() {
            e.actions[i] = Some(Action {
                root: Entry {
                    definition: 4,
                    generation: 1,
                    record: 48,
                },
                record: i as u16,
                sequence: 0,
                active: false,
                arguments: Extent { start, length },
                ticket: None,
                result: InvokeStatus::Invoked,
            });
        }
        for _ in 0..2 {
            e.compact();
            for (i, (start, expected)) in [
                (0, &[6, 7, 8, 9][..]),
                (2, &[8, 9][..]),
                (4, &[18, 19, 20][..]),
            ]
            .into_iter()
            .enumerate()
            {
                let r = e.actions[i].unwrap().arguments;
                assert_eq!((r.start, &e.bytes[r.range()]), (start, expected));
            }
            assert_eq!(e.extent(BYTES - 7).unwrap().start, 7);
            assert!(e.extent(BYTES - 6).is_err());
        }
    }
}
