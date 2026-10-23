//! Bounded native setpoint sessions. One latest pending target per session;
//! running native calls remain owned by the existing RTOS call tasks.
use crate::{
    ir::{Type, Value},
    kernel::appliance::{execution, Native},
    sync::TaskContext,
};
use lattice_peer_wire::{
    control::{Feedback, Progress, Request, State, Target, Ticket},
    program::transfer::{Reply, Status, DATA_BYTES, REQUEST_BYTES},
    InvokeStatus, StateReadResult,
};
const COUNT: usize = 4;
const IDLE_MS: u32 = 30_000;
#[derive(Clone, Copy)]
struct Session {
    caller: u64,
    nonce: u64,
    transformer: u64,
    attribute: u64,
    input: Type,
    touched: u32,
    target: Option<(u32, Value)>,
    pending: Option<(u32, Value)>,
    running: Option<(u32, execution::Ticket)>,
    completed: u32,
    result: InvokeStatus,
    state: StateReadResult,
    finishing: bool,
    released: bool,
    revision: u32,
    confirmed: u32,
    waiting: Option<(u16, u32, u32)>,
    due: u32,
    backoff: u8,
}
impl Session {
    fn progress(self) -> Progress {
        Progress {
            received: self.target.map_or(0, |t| t.0),
            completed: self.completed,
            result: self.result,
            finished: self.finishing && self.pending.is_none() && self.running.is_none(),
            state: self.state,
        }
    }
    fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}
#[derive(Clone, Copy)]
struct Slot {
    generation: u32,
    session: Option<Session>,
}
#[cfg(feature = "host-link")]
#[derive(Clone, Copy)]
struct Listener {
    owner: u64,
    nonce: u64,
    epoch: u64,
    generation: u32,
    revision: u32,
    ticket: Option<Ticket>,
    target: Option<Target>,
    received: u32,
    finishing: bool,
    touched: u32,
    waiting: Option<(u16, u32)>,
    due: u32,
    backoff: u8,
}
pub(super) struct Control {
    slots: [Slot; COUNT],
    turn: usize,
    canceled: [Option<(u16, u64)>; COUNT * 2],
    #[cfg(feature = "host-link")]
    listeners: [Option<Listener>; COUNT],
}
const _: () = assert!(core::mem::size_of::<Control>() <= 2048);

impl Control {
    pub const fn new() -> Self {
        Self {
            slots: [Slot {
                generation: 0,
                session: None,
            }; COUNT],
            turn: 0,
            canceled: [None; COUNT * 2],
            #[cfg(feature = "host-link")]
            listeners: [None; COUNT],
        }
    }
    #[cfg(feature = "host-link")]
    pub fn watch(&mut self, owner: u64, nonce: u64, epoch: u64, generation: u32, now: u32) -> bool {
        for (i, l) in self.listeners.iter_mut().enumerate() {
            if let Some(old) = l.filter(|l| super::host::session() != Some(l.generation)) {
                self.canceled[COUNT + i] = old.waiting.map(|(t, _)| (t, old.owner));
                *l = None;
            }
        }
        if let Some(l) = self
            .listeners
            .iter()
            .flatten()
            .find(|l| l.owner == owner && l.nonce == nonce)
        {
            return l.epoch == epoch && l.generation == generation;
        }
        let Some(slot) = self.listeners.iter_mut().find(|s| s.is_none()) else {
            return false;
        };
        *slot = Some(Listener {
            owner,
            nonce,
            epoch,
            generation,
            revision: 0,
            ticket: None,
            target: None,
            received: 0,
            finishing: false,
            touched: now,
            waiting: None,
            due: 0,
            backoff: 0,
        });
        true
    }
    #[cfg(feature = "host-link")]
    pub fn sealing(&mut self, owner: u64, ticket: Ticket, now: u32) {
        if let Some(l) = self
            .listeners
            .iter_mut()
            .flatten()
            .find(|l| l.owner == owner && l.ticket == Some(ticket))
        {
            l.finishing = true;
            l.touched = now;
        }
    }
    #[cfg(feature = "host-link")]
    pub fn unwatch(&mut self, owner: u64, nonce: u64) {
        for (i, l) in self.listeners.iter_mut().enumerate() {
            if let Some(old) = l.filter(|l| l.owner == owner && l.nonce == nonce) {
                self.canceled[COUNT + i] = old.waiting.map(|(t, _)| (t, owner));
                *l = None;
            }
        }
    }
    #[cfg(feature = "host-link")]
    pub fn offer(&mut self, t: Target, now: u32) -> Option<(u16, u64)> {
        #[cfg(feature = "firmware-update")]
        crate::kernel::update::activation::activity();
        let Some(l) = self.listeners.iter_mut().flatten().find(|l| {
            l.owner == t.owner && l.nonce == t.nonce && super::host::session() == Some(l.generation)
        }) else {
            if let Some(g) = super::host::session() {
                super::host::control_fault(g, t.owner, t.nonce, Status::Stale);
            }
            return None;
        };
        if t.sequence == 0
            || t.ticket.epoch != l.epoch
            || l.ticket.is_some_and(|old| old != t.ticket)
        {
            super::host::control_fault(l.generation, t.owner, t.nonce, Status::Invalid);
            return None;
        }
        if let Some(old) = l.target {
            if t.sequence < old.sequence {
                return None;
            }
            if t.sequence == old.sequence {
                if t.value != old.value {
                    super::host::control_fault(l.generation, t.owner, t.nonce, Status::Conflict);
                }
                return None;
            }
        }
        l.ticket = Some(t.ticket);
        l.target = Some(t);
        l.due = now;
        l.backoff = 0;
        l.touched = now;
        l.waiting.take().map(|(token, _)| (token, l.owner))
    }
    fn feedback(
        &mut self,
        caller: u64,
        nonce: u64,
        ticket: Ticket,
        revision: u32,
        progress: Progress,
    ) -> Status {
        #[cfg(feature = "host-link")]
        {
            let Some(l) = self.listeners.iter_mut().flatten().find(|l| {
                l.owner == caller
                    && l.nonce == nonce
                    && l.epoch == ticket.epoch
                    && super::host::session() == Some(l.generation)
            }) else {
                return Status::Stale;
            };
            if revision == 0 || l.ticket.is_some_and(|t| t != ticket) {
                return Status::Conflict;
            }
            l.ticket = Some(ticket);
            l.received = l.received.max(progress.received);
            if revision > l.revision {
                super::host::publish_control(
                    l.generation,
                    Feedback {
                        owner: caller,
                        nonce,
                        ticket,
                        revision,
                        progress,
                    },
                );
                l.revision = revision;
            }
            Status::Ok
        }
        #[cfg(not(feature = "host-link"))]
        {
            let _ = (caller, nonce, ticket, revision, progress);
            Status::Unsupported
        }
    }
    pub fn receive<'a>(
        &mut self,
        caller: u64,
        request: Request,
        epoch: u64,
        native: &'static dyn Native,
        context: &TaskContext,
        now: u32,
        out: &'a mut [u8; DATA_BYTES],
    ) -> Reply<'a> {
        if caller == 0 || caller == u64::MAX {
            return Reply::status(Status::Invalid);
        }
        #[cfg(feature = "firmware-update")]
        if matches!(
            request,
            Request::Open { .. } | Request::Target { .. } | Request::Finish { .. }
        ) {
            crate::kernel::update::activation::activity();
        }
        if let Request::Feedback {
            nonce,
            ticket,
            revision,
            progress,
        } = request
        {
            return Reply {
                generation: revision,
                ..Reply::status(self.feedback(caller, nonce, ticket, revision, progress))
            };
        }
        if epoch == 0 {
            return Reply::status(Status::Busy);
        }
        let ticket = match request {
            Request::Open {
                epoch: expected,
                nonce,
                transformer,
                state,
            } => {
                if expected != epoch {
                    return Reply::status(Status::Stale);
                }
                if nonce == 0 {
                    return Reply::status(Status::Invalid);
                }
                if let Some((i, slot)) = self.slots.iter_mut().enumerate().find(|(_, s)| {
                    s.session
                        .is_some_and(|s| s.caller == caller && s.nonce == nonce)
                }) {
                    let s = slot.session.as_mut().unwrap();
                    if s.released || s.transformer != transformer || s.attribute != state {
                        return Reply::status(Status::Conflict);
                    }
                    s.touched = now;
                    Ticket {
                        epoch,
                        slot: i as u16,
                        generation: slot.generation,
                    }
                } else {
                    let Some(&[input]) = native.transformer_signature(transformer) else {
                        return Reply::status(Status::Binding);
                    };
                    if native.state_binding(state).is_none() {
                        return Reply::status(Status::Binding);
                    }
                    let Some((i, slot)) = self
                        .slots
                        .iter_mut()
                        .enumerate()
                        .find(|(_, s)| s.session.is_none() && s.generation != u32::MAX)
                    else {
                        return Reply::status(Status::Busy);
                    };
                    slot.generation += 1;
                    slot.session = Some(Session {
                        caller,
                        nonce,
                        transformer,
                        attribute: state,
                        input,
                        touched: now,
                        target: None,
                        pending: None,
                        running: None,
                        completed: 0,
                        result: InvokeStatus::Busy,
                        state: native.read(context, state),
                        finishing: false,
                        released: false,
                        revision: 1,
                        confirmed: 0,
                        waiting: None,
                        due: now,
                        backoff: 0,
                    });
                    Ticket {
                        epoch,
                        slot: i as u16,
                        generation: slot.generation,
                    }
                }
            }
            Request::Target { ticket, .. }
            | Request::Finish { ticket, .. }
            | Request::Close { ticket, .. } => ticket,
            Request::Feedback { .. } => unreachable!(),
        };
        if ticket.epoch != epoch {
            return Reply::status(Status::Stale);
        }
        let Some(slot) = self
            .slots
            .get_mut(ticket.slot as usize)
            .filter(|s| s.generation == ticket.generation)
        else {
            return Reply::status(Status::Stale);
        };
        let Some(s) = slot
            .session
            .as_mut()
            .filter(|s| s.caller == caller && !s.released)
        else {
            return Reply::status(Status::Stale);
        };
        let status = match request {
            Request::Open { .. } => Status::Ok,
            Request::Target {
                sequence, value, ..
            } => {
                if value.ty() != s.input || sequence == 0 {
                    return Reply::status(Status::Invalid);
                }
                let old = s.target.map_or(0, |t| t.0);
                if sequence == old && s.target.unwrap().1 != value {
                    return Reply::status(Status::Conflict);
                }
                if sequence > old {
                    if s.finishing {
                        return Reply::status(Status::Conflict);
                    }
                    if s.revision == u32::MAX {
                        return Reply::status(Status::Bounds);
                    }
                    s.target = Some((sequence, value));
                    s.changed();
                    if s.running
                        .is_some_and(|(_, ticket)| execution::replace_target(ticket, value))
                    {
                        s.running.as_mut().unwrap().0 = sequence;
                        s.pending = None;
                    } else {
                        s.pending = s.target;
                    }
                }
                // Old retransmissions can report current progress but never restore old intent.
                Status::Accepted
            }
            Request::Finish { sequence, .. } => {
                if s.target.map_or(0, |t| t.0) != sequence {
                    return Reply::status(Status::Conflict);
                }
                if !s.finishing {
                    s.finishing = true;
                    s.changed();
                }
                if s.progress().finished {
                    Status::Ok
                } else {
                    Status::Busy
                }
            }
            Request::Close { nonce, .. } => {
                if nonce != s.nonce {
                    return Reply::status(Status::Conflict);
                }
                s.released = true;
                self.canceled[ticket.slot as usize] =
                    s.waiting.take().map(|(token, _, _)| (token, s.caller));
                if !s.finishing {
                    s.pending = None;
                    if s.running.is_some_and(|(_, t)| execution::cancel_target(t)) {
                        s.running = None;
                    }
                }
                if s.running.is_none() && s.pending.is_none() {
                    slot.session = None;
                }
                return Reply::status(Status::Ok);
            }
            Request::Feedback { .. } => unreachable!(),
        };
        s.touched = now;
        State {
            ticket,
            input: s.input,
            revision: s.revision,
            progress: s.progress(),
        }
        .reply(status, out)
    }
    pub fn poll(&mut self, native: &'static dyn Native, context: &TaskContext, now: u32) {
        #[cfg(feature = "host-link")]
        for (i, l) in self.listeners.iter_mut().enumerate() {
            if let Some(old) = *l {
                if super::host::session() != Some(old.generation) {
                    self.canceled[COUNT + i] = old.waiting.map(|(t, _)| (t, old.owner));
                    *l = None;
                } else if !old.finishing
                    && old.target.is_none_or(|t| t.sequence <= old.received)
                    && now.wrapping_sub(old.touched) >= IDLE_MS
                {
                    super::host::control_fault(old.generation, old.owner, old.nonce, Status::Stale);
                    self.canceled[COUNT + i] = old.waiting.map(|(t, _)| (t, old.owner));
                    *l = None;
                }
            }
        }
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let Some(s) = &mut slot.session else {
                continue;
            };
            if let Some((sequence, ticket)) = s.running {
                if let Some(result) = execution::take_result(ticket) {
                    s.running = None;
                    s.completed = sequence;
                    s.result = result;
                    if s.finishing && s.pending.is_none() {
                        s.touched = now;
                    }
                    s.changed();
                }
            }
            if now.wrapping_sub(s.touched) >= IDLE_MS {
                if !s.finishing {
                    s.pending = None;
                    s.released = true;
                    if s.running.is_some_and(|(_, t)| execution::cancel_target(t)) {
                        s.running = None;
                    }
                } else if s.progress().finished {
                    s.released = true;
                }
            }
            if s.released && s.running.is_none() && s.pending.is_none() {
                self.canceled[i] = s.waiting.map(|(t, _, _)| (t, s.caller));
                slot.session = None;
                continue;
            }
            if s.running.is_none() {
                if let Some((sequence, value)) = s.pending {
                    match execution::submit_target(native, context, s.transformer, value) {
                        Ok(ticket) => {
                            s.pending = None;
                            s.running = Some((sequence, ticket));
                        }
                        Err(InvokeStatus::Busy) => {}
                        Err(e) => {
                            s.pending = None;
                            s.completed = sequence;
                            s.result = e;
                            s.changed();
                        }
                    }
                }
            }
            let state = native.read(context, s.attribute);
            if !matches!(state, StateReadResult::Busy) && s.state != state {
                s.state = state;
                s.changed();
            }
        }
    }
    pub fn canceled(&mut self) -> Option<(u16, u64)> {
        self.canceled.iter_mut().find_map(Option::take)
    }
    pub fn pending(&self) -> bool {
        #[cfg(feature = "host-link")]
        if self
            .listeners
            .iter()
            .flatten()
            .any(|l| l.target.is_some_and(|t| t.sequence > l.received))
        {
            return true;
        }
        self.slots
            .iter()
            .filter_map(|s| s.session)
            .any(|s| s.running.is_some() || s.pending.is_some())
    }
    pub fn wait_ms(&self, now: u32) -> Option<u32> {
        if let Some((_, _, _, sent)) = self.awaiting() {
            return Some(200u32.saturating_sub(now.wrapping_sub(sent)));
        }
        let until = |due: u32| {
            if due.wrapping_sub(now) < 1 << 31 {
                due.wrapping_sub(now)
            } else {
                0
            }
        };
        let mut next = self
            .slots
            .iter()
            .filter_map(|s| s.session)
            .filter(|s| !s.released && s.revision != s.confirmed)
            .map(|s| until(s.due))
            .min();
        #[cfg(feature = "host-link")]
        for l in self.listeners.iter().flatten() {
            if l.target.is_some_and(|t| t.sequence > l.received) {
                next = Some(next.map_or(until(l.due), |n| n.min(until(l.due))));
            }
        }
        next
    }
    pub fn awaiting(&self) -> Option<(usize, u16, u64, u32)> {
        #[cfg(feature = "host-link")]
        for (i, l) in self.listeners.iter().enumerate() {
            if let Some(l) = l {
                if let Some((t, at)) = l.waiting {
                    return Some((COUNT + i, t, l.owner, at));
                }
            }
        }
        self.slots.iter().enumerate().find_map(|(i, slot)| {
            let s = slot.session?;
            let (t, at, _) = s.waiting?;
            Some((i, t, s.caller, at))
        })
    }
    pub fn outgoing(
        &mut self,
        epoch: u64,
        now: u32,
        out: &mut [u8; REQUEST_BYTES],
    ) -> Option<(usize, u64, usize)> {
        if self.awaiting().is_some() {
            return None;
        }
        for n in 0..COUNT * 2 {
            let i = (self.turn + n) % (COUNT * 2);
            #[cfg(feature = "host-link")]
            if i >= COUNT {
                let Some(l) = self.listeners[i - COUNT] else {
                    continue;
                };
                let Some(t) = l.target else {
                    continue;
                };
                if t.sequence <= l.received || now.wrapping_sub(l.due) >= 1 << 31 {
                    continue;
                }
                self.turn = (i + 1) % (COUNT * 2);
                return Some((
                    i,
                    l.owner,
                    Request::Target {
                        ticket: t.ticket,
                        sequence: t.sequence,
                        value: t.value,
                    }
                    .encode(out),
                ));
            }
            if i >= COUNT || epoch == 0 {
                continue;
            }
            let slot = &self.slots[i];
            let Some(s) = slot.session else {
                continue;
            };
            if s.released || s.confirmed == s.revision || now.wrapping_sub(s.due) >= 1 << 31 {
                continue;
            }
            let r = Request::Feedback {
                nonce: s.nonce,
                ticket: Ticket {
                    epoch,
                    slot: i as u16,
                    generation: slot.generation,
                },
                revision: s.revision,
                progress: s.progress(),
            };
            self.turn = (i + 1) % (COUNT * 2);
            return Some((i, s.caller, r.encode(out)));
        }
        None
    }
    pub fn sent(&mut self, index: usize, token: u16, now: u32) {
        #[cfg(feature = "host-link")]
        if index >= COUNT {
            if let Some(l) = self.listeners[index - COUNT].as_mut() {
                l.waiting = Some((token, now));
            }
            return;
        }
        if let Some(s) = self.slots[index].session.as_mut() {
            s.waiting = Some((token, now, s.revision));
        }
    }
    pub fn reply(&mut self, index: usize, reply: Reply<'_>, now: u32) {
        #[cfg(feature = "host-link")]
        if index >= COUNT {
            let Some(l) = self.listeners[index - COUNT] else {
                return;
            };
            if reply.status == Status::Busy {
                self.retry(index, now);
                return;
            }
            if reply.status == Status::Accepted {
                if let Some(state) = State::decode(reply).filter(|s| Some(s.ticket) == l.ticket) {
                    self.feedback(
                        l.owner,
                        l.nonce,
                        state.ticket,
                        state.revision,
                        state.progress,
                    );
                    if let Some(l) = self.listeners[index - COUNT].as_mut() {
                        l.waiting = None;
                        l.backoff = 0;
                        l.due = now;
                    }
                    return;
                }
            }
            super::host::control_fault(
                l.generation,
                l.owner,
                l.nonce,
                if reply.status == Status::Accepted {
                    Status::Invalid
                } else {
                    reply.status
                },
            );
            self.listeners[index - COUNT] = None;
            return;
        }
        let Some(s) = self.slots[index].session.as_mut() else {
            return;
        };
        if reply.status == Status::Ok && s.waiting.is_some_and(|(_, _, r)| r == reply.generation) {
            s.confirmed = reply.generation;
            s.waiting = None;
            s.backoff = 0;
            s.due = now;
        } else if matches!(reply.status, Status::Stale | Status::Unsupported) {
            // Listener disappeared. Stop feedback; accepted native/final work remains owned.
            s.waiting = None;
            s.confirmed = s.revision;
            s.released = true;
            if !s.finishing {
                s.pending = None;
                if s.running.is_some_and(|(_, t)| execution::cancel_target(t)) {
                    s.running = None;
                }
            }
        } else {
            self.retry(index, now);
        }
    }
    pub fn retry(&mut self, index: usize, now: u32) {
        #[cfg(feature = "host-link")]
        if index >= COUNT {
            if let Some(l) = self.listeners[index - COUNT].as_mut() {
                l.waiting = None;
                l.backoff = l.backoff.saturating_add(1).min(4);
                l.due = now.wrapping_add(25u32 << l.backoff);
            }
            return;
        }
        if let Some(s) = self.slots[index].session.as_mut() {
            s.waiting = None;
            s.backoff = s.backoff.saturating_add(1).min(5);
            s.due = now.wrapping_add(50u32 << s.backoff);
        }
    }
}

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(super) fn take() -> &'static mut Control {
    use core::sync::atomic::{AtomicBool, Ordering};
    static TAKEN: AtomicBool = AtomicBool::new(false);
    static mut CONTROL: Control = Control::new();
    assert!(
        !TAKEN.swap(true, Ordering::AcqRel),
        "control service already owned"
    );
    unsafe { &mut *core::ptr::addr_of_mut!(CONTROL) }
}
#[cfg(all(test, feature = "port-test"))]
mod tests;
