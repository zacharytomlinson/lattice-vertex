//! Bounded discovery, owner-directed reads/calls, and state-delta forwarding.
//! Return paths and duplicate records are communication state, never semantic
//! edges or queued graph activations.

use crate::kernel::radio::cost::{TimePerByte, Transmission};

use super::{
    exposure::Description,
    frame::{
        Content, Invocation, InvokeStatus, LinkPhase, LinkRead, Message, StateChange, StateRead,
        StateReadResult, SurfaceOwner, SurfaceRead, UpdateRead, CALL_ARGUMENT_BYTES, MAX_HOPS,
        SURFACE_BYTES,
    },
    links::Links,
    Network,
};

const ROUTES: usize = 3;
const READ_PATHS: usize = 8;
const DISCOVERY_PATHS: usize = 4;
const OWNER_NOTICES: usize = 8;
const CALL_PATHS: usize = 4;
const CHANGE_NOTICES: usize = 8;
const READ_MS: u32 = 10_000;
const ROUTE_MS: u32 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Direction {
    next: u64,
    confirmed: u32,
    tail_cost: TimePerByte,
    hops: u8,
}
impl Direction {
    const EMPTY: Self = Self {
        next: 0,
        confirmed: 0,
        tail_cost: TimePerByte::UNKNOWN,
        hops: 0,
    };
    fn rank_key(self, outgoing: TimePerByte) -> (TimePerByte, u8, u64) {
        (outgoing.then(self.tail_cost), self.hops, self.next)
    }
}

#[derive(Clone, Copy)]
struct Route {
    owner: u64,
    directions: [Direction; 2],
}
impl Route {
    const EMPTY: Self = Self {
        owner: 0,
        directions: [Direction::EMPTY; 2],
    };
    fn age(&self, now: u32) -> u32 {
        self.directions
            .iter()
            .filter(|d| d.next != 0)
            .map(|d| now.wrapping_sub(d.confirmed))
            .min()
            .unwrap_or(u32::MAX)
    }
    fn remove(&mut self, next: u64) {
        for d in &mut self.directions {
            if d.next == next {
                *d = Direction::EMPTY;
            }
        }
        self.clear_empty();
    }
    fn clear_empty(&mut self) {
        if self.directions.iter().all(|d| d.next == 0) {
            *self = Self::EMPTY;
        }
    }
    fn unanswered(&mut self, next: u64, started: u32, now: u32) {
        for d in &mut self.directions {
            if d.next == next && now.wrapping_sub(d.confirmed) >= now.wrapping_sub(started) {
                *d = Direction::EMPTY;
            }
        }
        self.clear_empty();
    }
}

/// All read kinds share one bounded reverse-path budget and token namespace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadRequest {
    Update(UpdateRead),
    Surface(SurfaceRead),
    State(StateRead),
    Links(LinkRead),
}
impl ReadRequest {
    pub(super) fn reader(self) -> u64 {
        match self {
            Self::Update(r) => r.reader,
            Self::Surface(r) => r.reader,
            Self::State(r) => r.reader,
            Self::Links(r) => r.reader,
        }
    }
    pub(super) fn owner(self) -> u64 {
        match self {
            Self::Update(r) => r.owner,
            Self::Surface(r) => r.owner,
            Self::State(r) => r.owner,
            Self::Links(r) => r.owner,
        }
    }
    fn request(self) -> u16 {
        match self {
            Self::Update(r) => r.request,
            Self::Surface(r) => r.request,
            Self::State(r) => r.request,
            Self::Links(r) => r.request,
        }
    }
    fn direction(self, local: u64) -> u64 {
        match self {
            Self::Links(r) if r.via != 0 && r.via != local => r.via,
            _ => self.owner(),
        }
    }
}

#[derive(Clone, Copy)]
struct ReadPath {
    read: ReadRequest,
    previous: u64,
    onward: u64,
    started: u32,
    answered: bool,
}
impl ReadPath {
    const EMPTY: Self = Self {
        read: ReadRequest::Surface(SurfaceRead {
            reader: 0,
            owner: 0,
            request: 0,
            offset: 0,
        }),
        previous: 0,
        onward: 0,
        started: 0,
        answered: false,
    };
}

#[derive(Clone, Copy)]
struct DiscoveryPath {
    seeker: u64,
    previous: u64,
    started: u32,
    request: u16,
}
impl DiscoveryPath {
    const EMPTY: Self = Self {
        seeker: 0,
        previous: 0,
        started: 0,
        request: 0,
    };
}

/// Duplicate history, not an owner catalog. Seeker zero denotes unsolicited
/// arrival; otherwise the key belongs to a particular owner-discovery request.
#[derive(Clone, Copy)]
struct OwnerNotice {
    owner: u64,
    seeker: u64,
    received: u32,
    token: u16,
}
impl OwnerNotice {
    const EMPTY: Self = Self {
        owner: 0,
        seeker: 0,
        received: 0,
        token: 0,
    };
}

#[derive(Clone, Copy)]
struct CallPath {
    call: Invocation,
    previous: [u64; 2],
    onward: [u64; 2],
    returned: [u64; 2],
    started: u32,
    arguments: [u8; CALL_ARGUMENT_BYTES],
    length: u8,
    remaining: u8,
    result: Option<(InvokeStatus, u8, TimePerByte)>,
}
impl CallPath {
    const EMPTY: Self = Self {
        call: Invocation {
            caller: 0,
            owner: 0,
            request: 0,
            transformer: 0,
        },
        previous: [0; 2],
        onward: [0; 2],
        returned: [0; 2],
        started: 0,
        arguments: [0; CALL_ARGUMENT_BYTES],
        length: 0,
        remaining: 0,
        result: None,
    };
}
#[derive(Clone, Copy)]
struct ChangeNotice {
    owner: u64,
    sequence: u32,
    received: u32,
}
impl ChangeNotice {
    const EMPTY: Self = Self {
        owner: 0,
        sequence: 0,
        received: 0,
    };
}

/// State owned by the resident peer task. Zero addresses mark empty records;
/// wire validation excludes them. Call paths retain exact arguments for replay
/// comparison; no foreign IR or appliance-state mirror is retained.
pub(super) struct Forwarder {
    local: u64,
    network: Network,
    links: Links,
    routes: [Route; ROUTES],
    reads: [ReadPath; READ_PATHS],
    discovery: [DiscoveryPath; DISCOVERY_PATHS],
    notices: [OwnerNotice; OWNER_NOTICES],
    calls: [CallPath; CALL_PATHS],
    changes: [ChangeNotice; CHANGE_NOTICES],
}

// Existing path budgets plus bounded per-neighbor update absence records.
// Peer, route, and return-path capacities are unchanged.
const _: () = assert!(core::mem::size_of::<Forwarder>() <= 2464);

impl Forwarder {
    pub(super) fn work_pending(&self) -> bool {
        self.reads
            .iter()
            .any(|r| r.read.reader() != 0 && !r.answered)
            || self
                .calls
                .iter()
                .any(|c| c.call.caller != 0 && c.result.is_none())
    }
    pub(super) fn record_update_absence(
        &mut self,
        peer: u64,
        notice: lattice_peer_wire::update::Absence,
        now: u32,
    ) {
        self.links.record_absence(peer, notice, now);
    }
    pub(super) fn activation_conflict(&self, contending: bool, now: u32) -> bool {
        self.links.activation_conflict(self.local, contending, now)
    }
    pub(super) const fn new(local: u64, network: Network) -> Self {
        Self {
            local,
            network,
            links: Links::new(),
            routes: [Route::EMPTY; ROUTES],
            reads: [ReadPath::EMPTY; READ_PATHS],
            discovery: [DiscoveryPath::EMPTY; DISCOVERY_PATHS],
            notices: [OwnerNotice::EMPTY; OWNER_NOTICES],
            calls: [CallPath::EMPTY; CALL_PATHS],
            changes: [ChangeNotice::EMPTY; CHANGE_NOTICES],
        }
    }

    /// Times are wrapping milliseconds. Traffic and the existing peer wakeup
    /// retire stale records; this is no additional scheduler or polling task.
    pub(super) fn expire(&mut self, now: u32) {
        let routes = &mut self.routes;
        self.links.expire(now, |peer| {
            for route in &mut *routes {
                route.remove(peer);
            }
        });
        for path in &mut self.calls {
            if path.call.caller != 0 && now.wrapping_sub(path.started) >= READ_MS {
                for next in path.onward {
                    if next != 0 && !path.returned.contains(&next) {
                        for route in &mut self.routes {
                            if route.owner == path.call.owner {
                                route.unanswered(next, path.started, now);
                            }
                        }
                    }
                }
                *path = CallPath::EMPTY;
            }
        }
        for change in &mut self.changes {
            if change.owner != 0 && now.wrapping_sub(change.received) >= READ_MS {
                *change = ChangeNotice::EMPTY;
            }
        }
        for path in &mut self.discovery {
            if path.seeker != 0 && now.wrapping_sub(path.started) >= READ_MS {
                *path = DiscoveryPath::EMPTY;
            }
        }
        for notice in &mut self.notices {
            if notice.owner != 0 && now.wrapping_sub(notice.received) >= READ_MS {
                *notice = OwnerNotice::EMPTY;
            }
        }
        for path in &mut self.reads {
            if path.read.reader() != 0 && now.wrapping_sub(path.started) >= READ_MS {
                if !path.answered && path.onward != 0 {
                    for route in &mut self.routes {
                        if route.owner == path.read.direction(self.local) {
                            route.unanswered(path.onward, path.started, now);
                        }
                    }
                }
                *path = ReadPath::EMPTY;
            }
        }
        for route in &mut self.routes {
            for d in &mut route.directions {
                if d.next != 0 && now.wrapping_sub(d.confirmed) >= ROUTE_MS {
                    *d = Direction::EMPTY;
                }
            }
            route.clear_empty();
        }
    }

    pub(super) fn record_transmission(&mut self, sample: Transmission, now: u32) {
        if sample.peer != self.local {
            self.links.record_transmission(sample, now);
        }
    }

    /// The same per-byte path cost supports ranking and later quantity estimates.
    pub(super) fn cost_to(&self, owner: u64) -> TimePerByte {
        if owner == self.local {
            return TimePerByte::ZERO;
        }
        let next = self.next_hops(owner, self.local, MAX_HOPS)[0];
        if next == 0 {
            return TimePerByte::UNKNOWN;
        }
        let tail = self
            .routes
            .iter()
            .find(|r| r.owner == owner)
            .and_then(|r| r.directions.iter().find(|d| d.next == next))
            .map_or(TimePerByte::UNKNOWN, |d| d.tail_cost);
        self.links.cost(next).then(if next == owner {
            TimePerByte::ZERO
        } else {
            tail
        })
    }

    fn response_cost(&self, message: Message<'_>) -> TimePerByte {
        let direct = match message.content {
            Content::OwnerFound { hops, .. }
            | Content::SurfacePart { hops, .. }
            | Content::Updated { hops, .. }
            | Content::InvocationResult { hops, .. }
            | Content::StateValue { hops, .. }
            | Content::LinkEntry { hops, .. } => hops == 1,
            _ => false,
        };
        self.links.cost(message.source).then(if direct {
            TimePerByte::ZERO
        } else {
            message.path_cost
        })
    }

    /// Only directed presence for this selected network introduces a candidate.
    pub(super) fn record_presence(&mut self, peer: u64, now: u32) {
        self.expire(now);
        if peer != self.local {
            self.links.record_presence(peer, now);
        }
    }

    pub(super) fn maintain_links(&mut self, now: u32) -> Option<Message<'static>> {
        self.expire(now);
        let (peer, token) = self.links.maintain(now)?;
        Some(self.link_message(peer, LinkPhase::Offer, token))
    }

    fn link_message(&self, peer: u64, phase: LinkPhase, token: u32) -> Message<'static> {
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source: self.local,
            destination: Some(peer),
            content: Content::LinkExchange {
                network: self.network,
                phase,
                token,
            },
        }
    }

    pub(super) fn receive_link(
        &mut self,
        incoming: Message<'_>,
        now: u32,
    ) -> Option<Message<'static>> {
        self.expire(now);
        let Content::LinkExchange {
            network,
            phase,
            token,
        } = incoming.content
        else {
            return None;
        };
        if network != self.network
            || incoming.destination != Some(self.local)
            || incoming.source == self.local
        {
            return None;
        }
        let was_linked = self.links.contains(incoming.source);
        let reply = self.links.receive(incoming.source, phase, token, now);
        if was_linked && !self.links.contains(incoming.source) {
            for route in &mut self.routes {
                route.remove(incoming.source);
            }
        }
        reply.map(|phase| self.link_message(incoming.source, phase, token))
    }

    #[cfg(feature = "host-link")]
    pub(super) fn local_links(&self, after: u64) -> (u32, Option<super::frame::PeerLink>) {
        self.links.read(after)
    }

    /// A local reader shares the same bounded read paths and route learning as
    /// relayed reads. It is not injected as a fictitious radio neighbor.
    #[cfg(feature = "host-link")]
    pub(super) fn originate_read(
        &mut self,
        request: ReadRequest,
        now: u32,
    ) -> Option<Message<'static>> {
        if matches!(request, ReadRequest::Update(_)) {
            return None;
        }
        let destination = self.originate_read_path(request, now)?;
        Some(Message {
            path_cost: TimePerByte::UNKNOWN,
            source: self.local,
            destination,
            content: match request {
                ReadRequest::Update(_) => return None, // payload-bearing origin uses originate_update
                ReadRequest::Links(read) => Content::ReadLinks {
                    network: self.network,
                    read,
                    remaining: MAX_HOPS,
                },
                ReadRequest::Surface(read) => Content::ReadSurface {
                    network: self.network,
                    read,
                    remaining: MAX_HOPS,
                },
                ReadRequest::State(read) => Content::ReadState {
                    network: self.network,
                    read,
                    remaining: MAX_HOPS,
                },
            },
        })
    }

    #[cfg(feature = "host-link")]
    fn originate_read_path(&mut self, request: ReadRequest, now: u32) -> Option<Option<u64>> {
        let reader = request.reader();
        let owner = request.owner();
        self.expire(now);
        if reader != self.local
            || owner == self.local
            || matches!(request, ReadRequest::Links(read) if read.via == self.local && !self.links.contains(owner))
            || self
                .reads
                .iter()
                .any(|p| p.read.reader() == self.local && p.read.request() == request.request())
        {
            return None;
        }
        let onward = self
            .next_hop(request.direction(self.local), self.local, MAX_HOPS)
            .unwrap_or(0);
        self.remember_read(request, self.local, onward, false, now);
        Some((onward != 0).then_some(onward))
    }

    #[cfg(feature = "host-link")]
    pub(super) fn originate_update<'a>(
        &mut self,
        read: UpdateRead,
        bytes: &'a [u8],
        now: u32,
    ) -> Option<Message<'a>> {
        let destination = self.originate_read_path(ReadRequest::Update(read), now)?;
        Some(Message {
            source: self.local,
            destination,
            path_cost: TimePerByte::UNKNOWN,
            content: Content::Update {
                network: self.network,
                read,
                remaining: MAX_HOPS,
                bytes,
            },
        })
    }

    pub(super) fn receive_update<'a>(
        &mut self,
        incoming: Message<'a>,
        now: u32,
        mut handle: impl FnMut(
            lattice_peer_wire::update::Request<'_>,
        ) -> lattice_peer_wire::update::Progress,
    ) -> Option<Message<'a>> {
        self.expire(now);
        match incoming.content {
            Content::Update {
                network,
                read,
                remaining,
                bytes,
            } if network == self.network => {
                let next = self
                    .begin_read(ReadRequest::Update(read), incoming.source, remaining, now)?
                    .onward;
                if read.owner == self.local {
                    let request = lattice_peer_wire::update::Request::decode(bytes)?;
                    return Some(Message {
                        source: self.local,
                        destination: Some(incoming.source),
                        path_cost: TimePerByte::ZERO,
                        content: Content::Updated {
                            network,
                            read,
                            hops: 1,
                            progress: handle(request),
                        },
                    });
                }
                Some(Message {
                    source: self.local,
                    destination: (next != 0).then_some(next),
                    path_cost: TimePerByte::UNKNOWN,
                    content: Content::Update {
                        network,
                        read,
                        remaining: remaining - 1,
                        bytes,
                    },
                })
            }
            Content::Updated {
                network,
                read,
                hops,
                progress,
            } if network == self.network && incoming.destination == Some(self.local) => {
                let previous = self.return_read(
                    ReadRequest::Update(read),
                    incoming.source,
                    hops,
                    now,
                    incoming.path_cost,
                )?;
                Some(Message {
                    source: self.local,
                    destination: Some(previous),
                    path_cost: self.response_cost(incoming),
                    content: Content::Updated {
                        network,
                        read,
                        hops: hops + 1,
                        progress,
                    },
                })
            }
            _ => None,
        }
    }

    #[cfg(feature = "host-link")]
    pub(super) fn complete_read<'a>(
        &mut self,
        incoming: Message<'a>,
        now: u32,
    ) -> Option<Message<'a>> {
        self.expire(now);
        let (network, request, hops) = match incoming.content {
            Content::Updated {
                network,
                read,
                hops,
                ..
            } => (network, ReadRequest::Update(read), hops),
            Content::LinkEntry {
                network,
                read,
                hops,
                ..
            } => (network, ReadRequest::Links(read), hops),
            Content::SurfacePart {
                network,
                read,
                hops,
                ..
            } => (network, ReadRequest::Surface(read), hops),
            Content::StateValue {
                network,
                read,
                hops,
                ..
            } => (network, ReadRequest::State(read), hops),
            _ => return None,
        };
        if network != self.network
            || request.reader() != self.local
            || incoming.destination != Some(self.local)
        {
            return None;
        }
        let previous = self.return_read(request, incoming.source, hops, now, incoming.path_cost)?;
        (previous == self.local).then_some(incoming)
    }

    #[cfg(feature = "host-link")]
    pub(super) fn cancel_read(&mut self, request: ReadRequest) {
        for path in &mut self.reads {
            if path.read == request && request.reader() == self.local {
                *path = ReadPath::EMPTY;
            }
        }
    }

    #[cfg(feature = "host-link")]
    pub(super) fn identity(&self) -> (u64, Network) {
        (self.local, self.network)
    }

    #[cfg(feature = "host-link")]
    pub(super) fn originate_call<'a>(
        &mut self,
        call: Invocation,
        arguments: &'a [u8],
        now: u32,
    ) -> Option<Message<'a>> {
        self.expire(now);
        if call.caller != self.local
            || call.owner == self.local
            || self
                .calls
                .iter()
                .any(|p| p.call.caller == self.local && p.call.request == call.request)
        {
            return None;
        }
        let next = self.next_hop(call.owner, self.local, MAX_HOPS).unwrap_or(0);
        self.remember_call(call, arguments, self.local, [next, 0], MAX_HOPS, now);
        Some(Message {
            path_cost: TimePerByte::UNKNOWN,
            source: self.local,
            destination: (next != 0).then_some(next),
            content: Content::Invoke {
                network: self.network,
                call,
                remaining: MAX_HOPS,
                copies: if next == 0 { 1 } else { 2 },
                arguments,
            },
        })
    }

    #[cfg(feature = "host-link")]
    pub(super) fn complete_call(
        &mut self,
        incoming: Message<'_>,
        now: u32,
    ) -> Option<(Invocation, InvokeStatus)> {
        if !matches!(incoming.content, Content::InvocationResult { call, .. } if call.caller == self.local)
            || incoming.destination != Some(self.local)
        {
            return None;
        }
        let replies = self.receive_control(incoming, now, |_, _| unreachable!());
        let Content::InvocationResult { call, status, .. } = replies[0]?.content else {
            return None;
        };
        Some((call, status))
    }

    #[cfg(feature = "host-link")]
    pub(super) fn cancel_call(&mut self, call: Invocation) {
        for path in &mut self.calls {
            if path.call == call && call.caller == self.local {
                *path = CallPath::EMPTY;
            }
        }
    }

    pub(super) fn receive_links(
        &mut self,
        incoming: Message<'_>,
        now: u32,
    ) -> Option<Message<'static>> {
        self.expire(now);
        match incoming.content {
            Content::ReadLinks {
                network,
                read,
                remaining,
            } => {
                if network != self.network {
                    return None;
                }
                // The advertising vertex can only hand off to a maintained
                // adjacent participant. A stale reference is not a new flood.
                if read.via == self.local && !self.links.contains(read.owner) {
                    return None;
                }
                let onward = self
                    .begin_read(ReadRequest::Links(read), incoming.source, remaining, now)?
                    .onward;
                if read.owner == self.local {
                    let (revision, link) = self.links.read(read.after);
                    return Some(Message {
                        path_cost: TimePerByte::ZERO,
                        source: self.local,
                        destination: Some(incoming.source),
                        content: Content::LinkEntry {
                            network,
                            read,
                            revision,
                            link,
                            hops: 1,
                        },
                    });
                }
                Some(Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: self.local,
                    destination: (onward != 0).then_some(onward),
                    content: Content::ReadLinks {
                        network,
                        read,
                        remaining: remaining - 1,
                    },
                })
            }
            Content::LinkEntry {
                network,
                read,
                revision,
                link,
                hops,
            } => {
                if network != self.network {
                    return None;
                }
                let previous = self.return_read(
                    ReadRequest::Links(read),
                    incoming.source,
                    hops,
                    now,
                    incoming.path_cost,
                )?;
                Some(Message {
                    path_cost: self.response_cost(incoming),
                    source: self.local,
                    destination: Some(previous),
                    content: Content::LinkEntry {
                        network,
                        read,
                        revision,
                        link,
                        hops: hops + 1,
                    },
                })
            }
            _ => None,
        }
    }

    /// At most two immediate transmissions. Copies keep the same invocation
    /// identity; a split consumes the budget so descendants cannot split again.
    pub(super) fn receive_control<'a>(
        &mut self,
        incoming: Message<'a>,
        now: u32,
        invoke: impl FnOnce(Invocation, &[u8]) -> InvokeStatus,
    ) -> [Option<Message<'a>>; 2] {
        self.expire(now);
        match incoming.content {
            Content::Invoke {
                network,
                call,
                remaining,
                copies,
                arguments,
            } => {
                if network != self.network || call.caller == self.local {
                    return [None; 2];
                }
                if call.owner == self.local {
                    let status = invoke(call, arguments);
                    return [
                        Some(Message {
                            path_cost: TimePerByte::ZERO,
                            source: self.local,
                            destination: Some(incoming.source),
                            content: Content::InvocationResult {
                                network,
                                call,
                                status,
                                hops: 1,
                            },
                        }),
                        None,
                    ];
                }
                if let Some(path) = self
                    .calls
                    .iter_mut()
                    .find(|p| p.call.caller == call.caller && p.call.request == call.request)
                {
                    if path.call != call
                        || &path.arguments[..path.length as usize] != arguments
                        || path.onward.contains(&incoming.source)
                    {
                        return [None; 2];
                    }
                    if !path.previous.contains(&incoming.source) {
                        // A copy returning from downstream has spent more hops.
                        // Only equal/shorter incoming branches join the return paths.
                        if remaining < path.remaining {
                            return [None; 2];
                        }
                        let Some(slot) = path.previous.iter_mut().find(|p| **p == 0) else {
                            return [None; 2];
                        };
                        *slot = incoming.source;
                    }
                    // A second incoming branch joins the retained return paths.
                    // Cached results answer just that copy; pending work is not forwarded again.
                    return [
                        path.result.map(|(status, hops, cost)| Message {
                            path_cost: cost,
                            source: self.local,
                            destination: Some(incoming.source),
                            content: Content::InvocationResult {
                                network,
                                call,
                                status,
                                hops: hops + 1,
                            },
                        }),
                        None,
                    ];
                }
                if remaining <= 1 {
                    return [None; 2];
                }
                let mut next = self.next_hops(call.owner, incoming.source, remaining);
                if copies == 1 {
                    next[1] = 0;
                }
                let split = next[1] != 0;
                // Broadcast exploration already fans out. It never grants each
                // recipient another directed fork of the same packet.
                let copies = if split || next[0] == 0 { 1 } else { copies };
                self.remember_call(call, arguments, incoming.source, next, remaining, now);
                let first = Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: self.local,
                    destination: (next[0] != 0).then_some(next[0]),
                    content: Content::Invoke {
                        network,
                        call,
                        remaining: remaining - 1,
                        copies,
                        arguments,
                    },
                };
                [
                    Some(first),
                    split.then_some(Message {
                        destination: Some(next[1]),
                        ..first
                    }),
                ]
            }
            Content::InvocationResult {
                network,
                call,
                status,
                hops,
            } => {
                if network != self.network || hops >= MAX_HOPS {
                    return [None; 2];
                }
                let response_cost = self.response_cost(incoming);
                let Some(path) = self.calls.iter_mut().find(|p| p.call == call) else {
                    return [None; 2];
                };
                if path.previous.contains(&incoming.source)
                    || path.returned.contains(&incoming.source)
                    || (path.onward[0] != 0 && !path.onward.contains(&incoming.source))
                    || path.result.is_some_and(|(old, _, _)| old != status)
                {
                    return [None; 2];
                }
                let Some(returned) = path.returned.iter_mut().find(|p| **p == 0) else {
                    return [None; 2];
                };
                *returned = incoming.source;
                if path.result.is_none_or(|(_, old, _)| hops <= old) {
                    path.result = Some((status, hops, response_cost));
                }
                let previous = path.previous;
                self.learn(call.owner, incoming.source, hops, now, incoming.path_cost);
                let first = Message {
                    path_cost: self.response_cost(incoming),
                    source: self.local,
                    destination: Some(previous[0]),
                    content: Content::InvocationResult {
                        network,
                        call,
                        status,
                        hops: hops + 1,
                    },
                };
                [
                    Some(first),
                    (previous[1] != 0).then_some(Message {
                        destination: Some(previous[1]),
                        ..first
                    }),
                ]
            }
            _ => [None; 2],
        }
    }

    fn remember_call(
        &mut self,
        call: Invocation,
        arguments: &[u8],
        previous: u64,
        next: [u64; 2],
        remaining: u8,
        now: u32,
    ) {
        let slot = self
            .calls
            .iter()
            .position(|p| p.call.caller == 0)
            .unwrap_or_else(|| {
                (0..CALL_PATHS)
                    .max_by_key(|&i| now.wrapping_sub(self.calls[i].started))
                    .unwrap()
            });
        let mut path = CallPath {
            call,
            previous: [previous, 0],
            onward: next,
            started: now,
            length: arguments.len() as u8,
            remaining,
            ..CallPath::EMPTY
        };
        path.arguments[..arguments.len()].copy_from_slice(arguments);
        self.calls[slot] = path;
    }

    pub(super) fn announce_change(
        &mut self,
        attribute: u64,
        sequence: u32,
        value: Option<crate::ir::Value>,
        now: u32,
    ) -> Message<'static> {
        self.expire(now);
        self.change_seen(self.local, sequence, now);
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source: self.local,
            destination: None,
            content: Content::StateChanged {
                network: self.network,
                delta: StateChange {
                    owner: self.local,
                    attribute,
                    sequence,
                    value,
                },
                hops: 1,
            },
        }
    }

    pub(super) fn receive_change(
        &mut self,
        incoming: Message<'_>,
        now: u32,
        mut send: impl FnMut(Message<'static>),
    ) -> Option<StateChange> {
        self.expire(now);
        let Content::StateChanged {
            network,
            delta,
            hops,
        } = incoming.content
        else {
            return None;
        };
        if network != self.network
            || delta.owner == self.local
            || !self.change_seen(delta.owner, delta.sequence, now)
        {
            return None;
        }
        if hops < MAX_HOPS {
            send(Message {
                path_cost: TimePerByte::UNKNOWN,
                source: self.local,
                destination: None,
                content: Content::StateChanged {
                    network,
                    delta,
                    hops: hops + 1,
                },
            });
        }
        Some(delta)
    }

    fn change_seen(&mut self, owner: u64, sequence: u32, now: u32) -> bool {
        if self
            .changes
            .iter()
            .any(|c| c.owner == owner && c.sequence == sequence)
        {
            return false;
        }
        let slot = self
            .changes
            .iter()
            .position(|c| c.owner == 0)
            .unwrap_or_else(|| {
                (0..CHANGE_NOTICES)
                    .max_by_key(|&i| now.wrapping_sub(self.changes[i].received))
                    .unwrap()
            });
        self.changes[slot] = ChangeNotice {
            owner,
            sequence,
            received: now,
        };
        true
    }

    pub(super) fn announce(
        &mut self,
        sequence: u16,
        surface_bytes: u16,
        now: u32,
    ) -> Message<'static> {
        self.expire(now);
        self.notice(self.local, 0, sequence, now);
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source: self.local,
            destination: None,
            content: Content::Arrive {
                network: self.network,
                owner: SurfaceOwner {
                    address: self.local,
                    surface_bytes,
                },
                sequence,
                hops: 1,
            },
        }
    }

    pub(super) fn find_owners(&mut self, request: u16, now: u32) -> Message<'static> {
        self.expire(now);
        self.remember_discovery(self.local, request, 0, now);
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source: self.local,
            destination: None,
            content: Content::FindOwners {
                network: self.network,
                seeker: self.local,
                request,
                remaining: MAX_HOPS,
            },
        }
    }

    /// Discovery can emit the local owner reply and one outward exploration.
    /// Results are consumed by the caller as they arrive, never accumulated into
    /// a network inventory. Sending a control frame does not retain its bytes.
    pub(super) fn receive_discovery(
        &mut self,
        incoming: Message<'_>,
        surface_bytes: u16,
        now: u32,
        mut send: impl FnMut(Message<'static>),
    ) -> Option<SurfaceOwner> {
        self.expire(now);
        match incoming.content {
            Content::Arrive {
                network,
                owner,
                sequence,
                hops,
            } => {
                if network != self.network
                    || owner.address == self.local
                    || !self.notice(owner.address, 0, sequence, now)
                {
                    return None;
                }
                if hops < MAX_HOPS {
                    send(Message {
                        path_cost: TimePerByte::UNKNOWN,
                        source: self.local,
                        destination: None,
                        content: Content::Arrive {
                            network,
                            owner,
                            sequence,
                            hops: hops + 1,
                        },
                    });
                }
                // Hearing an announcement reveals an owner; it does not prove
                // a working return direction, so it does not install a route.
                Some(owner)
            }
            Content::FindOwners {
                network,
                seeker,
                request,
                remaining,
            } => {
                if network != self.network
                    || seeker == self.local
                    || self
                        .discovery
                        .iter()
                        .any(|p| p.seeker == seeker && p.request == request)
                {
                    return None;
                }
                self.remember_discovery(seeker, request, incoming.source, now);
                self.notice(self.local, seeker, request, now);
                send(Message {
                    path_cost: TimePerByte::ZERO,
                    source: self.local,
                    destination: Some(incoming.source),
                    content: Content::OwnerFound {
                        network,
                        seeker,
                        request,
                        owner: SurfaceOwner {
                            address: self.local,
                            surface_bytes,
                        },
                        hops: 1,
                    },
                });
                if remaining > 1 {
                    send(Message {
                        path_cost: TimePerByte::UNKNOWN,
                        source: self.local,
                        destination: None,
                        content: Content::FindOwners {
                            network,
                            seeker,
                            request,
                            remaining: remaining - 1,
                        },
                    });
                }
                None
            }
            Content::OwnerFound {
                network,
                seeker,
                request,
                owner,
                hops,
            } => {
                if network != self.network || owner.address == self.local {
                    return None;
                }
                let path = *self
                    .discovery
                    .iter()
                    .find(|p| p.seeker == seeker && p.request == request)?;
                if incoming.source == path.previous || (seeker != self.local && hops >= MAX_HOPS) {
                    return None;
                }
                let first = self.notice(owner.address, seeker, request, now);
                if first
                    || !self.routes.iter().any(|r| {
                        r.owner == owner.address
                            && r.directions.iter().any(|d| d.next == incoming.source)
                    })
                {
                    self.learn(
                        owner.address,
                        incoming.source,
                        hops,
                        now,
                        incoming.path_cost,
                    );
                }
                if !first {
                    return None;
                }
                if seeker != self.local {
                    send(Message {
                        path_cost: self.response_cost(incoming),
                        source: self.local,
                        destination: Some(path.previous),
                        content: Content::OwnerFound {
                            network,
                            seeker,
                            request,
                            owner,
                            hops: hops + 1,
                        },
                    });
                }
                // The path stays open for other owners' replies to this query.
                Some(owner)
            }
            _ => None,
        }
    }

    fn remember_discovery(&mut self, seeker: u64, request: u16, previous: u64, now: u32) {
        let slot = self
            .discovery
            .iter()
            .position(|p| p.seeker == 0)
            .unwrap_or_else(|| {
                (0..DISCOVERY_PATHS)
                    .max_by_key(|&i| now.wrapping_sub(self.discovery[i].started))
                    .unwrap()
            });
        self.discovery[slot] = DiscoveryPath {
            seeker,
            previous,
            started: now,
            request,
        };
    }

    fn notice(&mut self, owner: u64, seeker: u64, token: u16, now: u32) -> bool {
        if self
            .notices
            .iter()
            .any(|n| (n.owner, n.seeker, n.token) == (owner, seeker, token))
        {
            return false;
        }
        let slot = self
            .notices
            .iter()
            .position(|n| n.owner == 0)
            .unwrap_or_else(|| {
                (0..OWNER_NOTICES)
                    .max_by_key(|&i| now.wrapping_sub(self.notices[i].received))
                    .unwrap()
            });
        self.notices[slot] = OwnerNotice {
            owner,
            seeker,
            token,
            received: now,
        };
        true
    }

    /// Consume an already validated frame and produce at most one transmission.
    /// A request's first incoming neighbor fixes its reverse path. A fresh
    /// request token is required to retry a lost read within the recent window.
    pub(super) fn receive_surface<'a>(
        &mut self,
        incoming: Message<'a>,
        description: &Description,
        reply: &'a mut [u8; SURFACE_BYTES],
        now: u32,
    ) -> Option<Message<'a>> {
        self.expire(now);
        match incoming.content {
            Content::ReadSurface {
                network,
                read,
                remaining,
            } => {
                if network != self.network {
                    return None;
                }
                let owner = read.owner == self.local;
                if owner && read.offset > description.len() {
                    return None;
                }
                let onward = self
                    .begin_read(ReadRequest::Surface(read), incoming.source, remaining, now)?
                    .onward;
                let next = (onward != 0).then_some(onward);
                if owner {
                    let n = description.encode_part(read.offset, reply);
                    return Some(Message {
                        path_cost: TimePerByte::ZERO,
                        source: self.local,
                        destination: Some(incoming.source),
                        content: Content::SurfacePart {
                            network,
                            read,
                            total: description.len(),
                            hops: 1,
                            bytes: &reply[..n],
                        },
                    });
                }
                return Some(Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: self.local,
                    destination: next,
                    content: Content::ReadSurface {
                        network,
                        read,
                        remaining: remaining - 1,
                    },
                });
            }
            Content::SurfacePart {
                network,
                read,
                total,
                hops,
                bytes,
            } => {
                if network != self.network {
                    return None;
                }
                let previous = self.return_read(
                    ReadRequest::Surface(read),
                    incoming.source,
                    hops,
                    now,
                    incoming.path_cost,
                )?;
                return Some(Message {
                    path_cost: self.response_cost(incoming),
                    source: self.local,
                    destination: Some(previous),
                    content: Content::SurfacePart {
                        network,
                        read,
                        total,
                        hops: hops + 1,
                        bytes,
                    },
                });
            }
            _ => return None,
        }
    }

    /// Read current state at its owner; relays retain directions but no value cache.
    pub(super) fn receive_state(
        &mut self,
        incoming: Message<'_>,
        now: u32,
        read_native: impl FnOnce(u64) -> StateReadResult,
    ) -> Option<Message<'static>> {
        self.expire(now);
        match incoming.content {
            Content::ReadState {
                network,
                read,
                remaining,
            } => {
                if network != self.network {
                    return None;
                }
                let onward = self
                    .begin_read(ReadRequest::State(read), incoming.source, remaining, now)?
                    .onward;
                let next = (onward != 0).then_some(onward);
                if read.owner == self.local {
                    return Some(Message {
                        path_cost: TimePerByte::ZERO,
                        source: self.local,
                        destination: Some(incoming.source),
                        content: Content::StateValue {
                            network,
                            read,
                            result: read_native(read.attribute),
                            hops: 1,
                        },
                    });
                }
                Some(Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: self.local,
                    destination: next,
                    content: Content::ReadState {
                        network,
                        read,
                        remaining: remaining - 1,
                    },
                })
            }
            Content::StateValue {
                network,
                read,
                result,
                hops,
            } => {
                if network != self.network {
                    return None;
                }
                let previous = self.return_read(
                    ReadRequest::State(read),
                    incoming.source,
                    hops,
                    now,
                    incoming.path_cost,
                )?;
                Some(Message {
                    path_cost: self.response_cost(incoming),
                    source: self.local,
                    destination: Some(previous),
                    content: Content::StateValue {
                        network,
                        read,
                        result,
                        hops: hops + 1,
                    },
                })
            }
            _ => None,
        }
    }

    /// Retain the first incoming direction. Owners also remember the request to
    /// suppress duplicate reads; retrying any read kind requires a fresh token.
    fn begin_read(
        &mut self,
        read: ReadRequest,
        previous: u64,
        remaining: u8,
        now: u32,
    ) -> Option<&ReadPath> {
        if read.reader() == self.local
            || self
                .reads
                .iter()
                .any(|p| p.read.reader() == read.reader() && p.read.request() == read.request())
        {
            return None;
        }
        let owner = read.owner() == self.local;
        if !owner && remaining <= 1 {
            return None;
        }
        let next = if owner {
            None
        } else {
            self.next_hop(read.direction(self.local), previous, remaining)
        };
        Some(self.remember_read(read, previous, next.unwrap_or(0), owner, now))
    }

    fn remember_read(
        &mut self,
        read: ReadRequest,
        previous: u64,
        onward: u64,
        answered: bool,
        now: u32,
    ) -> &ReadPath {
        let slot = self
            .reads
            .iter()
            .position(|p| p.read.reader() == 0)
            .unwrap_or_else(|| {
                (0..READ_PATHS)
                    .max_by_key(|&i| now.wrapping_sub(self.reads[i].started))
                    .unwrap()
            });
        self.reads[slot] = ReadPath {
            read,
            previous,
            onward,
            started: now,
            answered,
        };
        &self.reads[slot]
    }

    fn return_read(
        &mut self,
        read: ReadRequest,
        source: u64,
        hops: u8,
        now: u32,
        tail_cost: TimePerByte,
    ) -> Option<u64> {
        if hops >= MAX_HOPS {
            return None;
        }
        let path = self.reads.iter_mut().find(|p| p.read == read)?;
        if path.answered || source == path.previous || (path.onward != 0 && path.onward != source) {
            return None;
        }
        path.answered = true;
        let previous = path.previous;
        // A correlated response confirms reachability, including Busy/UnknownAttribute.
        self.learn(read.owner(), source, hops, now, tail_cost);
        Some(previous)
    }

    fn next_hops(&self, owner: u64, previous: u64, remaining: u8) -> [u64; 2] {
        let mut candidates = [Direction::EMPTY; 3];
        if let Some(route) = self.routes.iter().find(|r| r.owner == owner) {
            candidates[..2].copy_from_slice(&route.directions);
        }
        if remaining > 1 && owner != previous && self.links.contains(owner) {
            candidates[2] = Direction {
                next: owner,
                hops: 1,
                tail_cost: TimePerByte::ZERO,
                confirmed: 0,
            };
        }
        candidates.sort_unstable_by_key(|d| d.rank_key(self.links.cost(d.next)));
        let mut selected = [0; 2];
        let mut count = 0;
        for d in candidates {
            if count == 2 {
                break;
            }
            if d.next != 0
                && d.next != previous
                && d.hops < remaining
                && !selected.contains(&d.next)
            {
                selected[count] = d.next;
                count += 1;
            }
        }
        selected
    }

    fn next_hop(&self, owner: u64, previous: u64, remaining: u8) -> Option<u64> {
        let next = self.next_hops(owner, previous, remaining)[0];
        (next != 0).then_some(next)
    }

    fn learn(&mut self, owner: u64, next: u64, hops: u8, now: u32, tail_cost: TimePerByte) {
        let slot = self
            .routes
            .iter()
            .position(|r| r.owner == owner)
            .or_else(|| self.routes.iter().position(|r| r.owner == 0))
            .unwrap_or_else(|| {
                (0..ROUTES)
                    .max_by_key(|&i| self.routes[i].age(now))
                    .unwrap()
            });
        let route = &mut self.routes[slot];
        if route.owner != owner {
            *route = Route {
                owner,
                ..Route::EMPTY
            };
        }
        let measured = Direction {
            next,
            hops,
            confirmed: now,
            tail_cost: if hops == 1 {
                TimePerByte::ZERO
            } else {
                tail_cost
            },
        };
        let index = route
            .directions
            .iter()
            .position(|d| d.next == next)
            .or_else(|| route.directions.iter().position(|d| d.next == 0));
        if let Some(index) = index {
            route.directions[index] = measured;
        } else {
            let worst = if route.directions[0].rank_key(self.links.cost(route.directions[0].next))
                > route.directions[1].rank_key(self.links.cost(route.directions[1].next))
            {
                0
            } else {
                1
            };
            if measured.rank_key(self.links.cost(next))
                < route.directions[worst].rank_key(self.links.cost(route.directions[worst].next))
            {
                route.directions[worst] = measured;
            }
        }
    }

    pub(super) fn transmit_failed(&mut self, outgoing: &Message<'_>) {
        let owner = match outgoing.content {
            Content::ReadSurface { read, .. } => read.owner,
            Content::Update { read, .. } => read.owner,
            Content::ReadState { read, .. } => read.owner,
            Content::ReadLinks { read, .. } => ReadRequest::Links(read).direction(self.local),
            Content::Invoke { call, .. } => call.owner,
            _ => return,
        };
        for route in &mut self.routes {
            if route.owner == owner {
                if let Some(next) = outgoing.destination {
                    route.remove(next);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use self::std::vec::Vec;
    use super::*;
    use crate::{
        kernel::radio::MAX_FRAME_SIZE,
        surface::{State, StateHandle, Surface},
    };

    const NET: Network = Network {
        founder: 1,
        channel: 15,
    };

    #[test]
    #[cfg(feature = "host-link")]
    fn update_ranges_use_read_paths_and_correlate_package_replies() {
        use lattice_peer_wire::update::{Phase, Progress, Request, Status, REQUEST_BYTES};
        let mut origin = Forwarder::new(1, NET);
        let mut relay = Forwarder::new(2, NET);
        let mut owner = Forwarder::new(3, NET);
        let read = UpdateRead {
            reader: 1,
            owner: 3,
            request: 42,
            package: [9; 16],
        };
        let mut payload = [0; REQUEST_BYTES];
        let data = [0x55; 48];
        let n = Request::Data {
            package: read.package,
            offset: 4096,
            data: &data,
        }
        .encode(&mut payload);
        let message = origin.originate_update(read, &payload[..n], 0).unwrap();
        let mut wire = [0; MAX_FRAME_SIZE];
        let n = message.encode(1, &mut wire);
        assert!(n <= MAX_FRAME_SIZE);
        let incoming = Message::decode(&wire[..n], 2, NET.channel).unwrap();
        let message = relay
            .receive_update(incoming, 1, |_| panic!("relay executed update"))
            .unwrap();
        let mut second = [0; MAX_FRAME_SIZE];
        let n = message.encode(2, &mut second);
        let incoming = Message::decode(&second[..n], 3, NET.channel).unwrap();
        let message = owner
            .receive_update(incoming, 2, |request| {
                assert_eq!(
                    request,
                    Request::Data {
                        package: read.package,
                        offset: 4096,
                        data: &data
                    }
                );
                Progress {
                    package: read.package,
                    phase: Phase::Receiving,
                    status: Status::Ok,
                    accepted: 4144,
                    committed: 4096,
                    prepared: 0,
                }
            })
            .unwrap();
        assert!(owner
            .receive_update(incoming, 3, |_| panic!("duplicate path dispatched"))
            .is_none());
        let mut third = [0; MAX_FRAME_SIZE];
        let n = message.encode(3, &mut third);
        let incoming = Message::decode(&third[..n], 2, NET.channel).unwrap();
        let message = relay
            .receive_update(incoming, 4, |_| panic!("reply dispatched update"))
            .unwrap();
        let n = message.encode(4, &mut wire);
        let incoming = Message::decode(&wire[..n], 1, NET.channel).unwrap();
        let mut unrelated = incoming;
        if let Content::Updated {
            ref mut read,
            ref mut progress,
            ..
        } = unrelated.content
        {
            read.package = [8; 16];
            progress.package = read.package;
        }
        assert!(origin.complete_read(unrelated, 5).is_none());
        let result = origin.complete_read(incoming, 5).unwrap();
        assert!(
            matches!(result.content,Content::Updated{read:r,progress,..} if r==read && progress.committed==4096)
        );
    }
    struct On;
    impl State for On {
        type Appliance = ();
        type Value = bool;
        fn read(_: &()) -> Option<bool> {
            panic!("a surface read must not read appliance state")
        }
    }
    static ON: StateHandle<()> = StateHandle::new::<On>();
    // A description spanning multiple radio frames; no native reader is called.
    static SURFACE: Surface<'static, (), ()> = Surface {
        states: &[&ON; 16],
        transformers: &[],
        axioms: &[],
        invariants: &[],
    };

    fn query(request: u16, offset: u16) -> Message<'static> {
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: None,
            content: Content::ReadSurface {
                network: NET,
                read: SurfaceRead {
                    reader: 1,
                    owner: 4,
                    request,
                    offset,
                },
                remaining: MAX_HOPS,
            },
        }
    }

    fn reply(request: u16, offset: u16) -> Message<'static> {
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 3,
            destination: Some(2),
            content: Content::SurfacePart {
                network: NET,
                read: SurfaceRead {
                    reader: 1,
                    owner: 4,
                    request,
                    offset,
                },
                total: 13,
                hops: 2,
                bytes: &[0; 13],
            },
        }
    }

    /// Exercise real encoded frames through local forwarding state. Links only
    /// determine who hears a transmission; no RF model or scheduler is simulated.
    fn exchange(
        nodes: &mut [Forwarder],
        links: &[(u64, u64)],
        initial: Message<'_>,
        now: u32,
    ) -> (Vec<u8>, Vec<(u64, Option<u64>)>, Vec<(u64, SurfaceOwner)>) {
        let description = Description::new(&SURFACE).unwrap();
        let mut bytes = [0; MAX_FRAME_SIZE];
        let length = initial.encode(0, &mut bytes);
        let mut pending = self::std::vec![(initial.source, initial.destination, bytes, length)];
        let mut reads = Vec::new();
        let mut answer = Vec::new();
        let mut owners = Vec::new();
        let mut cursor = 0;
        while cursor < pending.len() {
            assert!(pending.len() < 64, "duplicate forwarding did not terminate");
            let (sender, destination, wire, length) = pending[cursor];
            cursor += 1;
            if destination == Some(1)
                && links
                    .iter()
                    .any(|&(a, b)| (a, b) == (sender, 1) || (a, b) == (1, sender))
            {
                if let Content::SurfacePart { read, bytes, .. } =
                    Message::decode(&wire[..length], 1, 15).unwrap().content
                {
                    assert_eq!((read.reader, read.owner), (1, 4));
                    assert!(answer.is_empty(), "one request produced duplicate delivery");
                    answer.extend_from_slice(bytes);
                }
            }
            for node in nodes.iter_mut() {
                let local = node.local;
                if local == sender
                    || destination.is_some_and(|d| d != local)
                    || !links
                        .iter()
                        .any(|&(a, b)| (a, b) == (sender, local) || (a, b) == (local, sender))
                {
                    continue;
                }
                let incoming = Message::decode(&wire[..length], local, 15).unwrap();
                let mut piece = [0; SURFACE_BYTES];
                let mut transmit = |out: Message<'_>| {
                    if matches!(out.content, Content::ReadSurface { .. }) {
                        reads.push((local, out.destination));
                    }
                    let mut wire = [0; MAX_FRAME_SIZE];
                    let n = out.encode(cursor as u8, &mut wire);
                    pending.push((local, out.destination, wire, n));
                };
                if let Some(owner) =
                    node.receive_discovery(incoming, description.len(), now, &mut transmit)
                {
                    owners.push((local, owner));
                }
                if let Some(out) = node.receive_surface(incoming, &description, &mut piece, now) {
                    transmit(out);
                }
            }
        }
        (answer, reads, owners)
    }

    fn connect(nodes: &mut [Forwarder], a: usize, b: usize, now: u32) {
        let peer = nodes[b].local;
        nodes[a].record_presence(peer, now);
        let offer = nodes[a].maintain_links(now).unwrap();
        assert_eq!(offer.destination, Some(peer));
        let mut wire = [0; MAX_FRAME_SIZE];
        let n = offer.encode(0, &mut wire);
        let accept = nodes[b]
            .receive_link(Message::decode(&wire[..n], peer, 15).unwrap(), now + 1)
            .unwrap();
        let n = accept.encode(1, &mut wire);
        let local = nodes[a].local;
        let confirm = nodes[a]
            .receive_link(Message::decode(&wire[..n], local, 15).unwrap(), now + 2)
            .unwrap();
        let n = confirm.encode(2, &mut wire);
        assert!(nodes[b]
            .receive_link(Message::decode(&wire[..n], peer, 15).unwrap(), now + 3)
            .is_none());
    }

    /// A physical chain 1--2--3--4--5. The tablet at 1 only contacts 2 directly.
    fn walk(nodes: &mut [Forwarder], query: Message<'static>, now: u32) -> Message<'static> {
        let mut message = query;
        let mut wire = [0; MAX_FRAME_SIZE];
        let mut count = 0;
        loop {
            count += 1;
            assert!(count <= 8, "walk did not terminate");
            let target = message
                .destination
                .expect("guided walk unexpectedly flooded");
            assert_eq!(
                message.source.abs_diff(target),
                1,
                "walk skipped a physical link"
            );
            let n = message.encode(count, &mut wire);
            let incoming = Message::decode(&wire[..n], target, 15).unwrap();
            if target == 1 {
                assert_eq!(incoming, message);
                return message;
            }
            let node = nodes.iter_mut().find(|p| p.local == target).unwrap();
            message = node.receive_links(incoming, now).unwrap();
        }
    }

    #[test]
    fn alternate_directions_rank_marginal_cost_and_fail_independently() {
        let mut node = Forwarder::new(2, NET);
        for peer in 3..=6 {
            node.record_transmission(
                Transmission {
                    peer,
                    elapsed_us: 1000,
                    acknowledged_bytes: 1000,
                },
                0,
            );
        }
        for (next, hops, unit_us, expected) in [
            (3, 3, 41, [3, 0]),
            (4, 2, 40, [4, 3]),
            (5, 4, 10, [5, 4]),
            (6, 2, 99, [5, 4]),
            (4, 2, 5, [4, 5]),
            (4, 2, 80, [5, 4]),
        ] {
            node.learn(9, next, hops, 0, TimePerByte::measured(unit_us, 1));
            assert_eq!(node.next_hops(9, 1, 8), expected);
        }
        assert_eq!(node.next_hops(9, 5, 8), [4, 0]);
        assert_eq!(node.next_hops(9, 1, 2), [0, 0]);
        let failed = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 2,
            destination: Some(5),
            content: Content::ReadState {
                network: NET,
                read: StateRead {
                    reader: 1,
                    owner: 9,
                    request: 1,
                    attribute: 42,
                },
                remaining: 7,
            },
        };
        node.transmit_failed(&failed);
        assert_eq!(node.next_hops(9, 1, 8), [4, 0]);
        node.learn(9, 5, 2, 100, TimePerByte::measured(10, 1));
        node.expire(ROUTE_MS);
        assert_eq!(
            node.next_hops(9, 1, 8),
            [5, 0],
            "expiry erased a newer alternate"
        );
        // Owner capacity stays bounded independently of candidates per owner.
        node.learn(10, 3, 2, ROUTE_MS + 1, TimePerByte::measured(3, 1));
        node.learn(11, 3, 2, ROUTE_MS + 2, TimePerByte::measured(3, 1));
        node.learn(12, 3, 2, ROUTE_MS + 3, TimePerByte::measured(3, 1));
        assert_eq!(node.next_hops(9, 1, 8), [0, 0]);
        assert_eq!(node.next_hops(12, 1, 8), [3, 0]);
    }

    #[test]
    #[cfg(feature = "host-link")]
    fn directional_cost_crosses_encoded_replies_and_drives_quantity_estimates() {
        let mut a = Forwarder::new(1, NET);
        let mut b = Forwarder::new(2, NET);
        let mut c = Forwarder::new(3, NET);
        a.record_transmission(
            Transmission {
                peer: 2,
                elapsed_us: 1000,
                acknowledged_bytes: 100,
            },
            0,
        );
        b.record_transmission(
            Transmission {
                peer: 3,
                elapsed_us: 2000,
                acknowledged_bytes: 100,
            },
            0,
        );
        // The expensive reverse direction must not become the forward path price.
        b.record_transmission(
            Transmission {
                peer: 1,
                elapsed_us: 90000,
                acknowledged_bytes: 100,
            },
            0,
        );
        c.record_transmission(
            Transmission {
                peer: 2,
                elapsed_us: 80000,
                acknowledged_bytes: 100,
            },
            0,
        );
        let read = ReadRequest::State(StateRead {
            reader: 1,
            owner: 3,
            request: 7,
            attribute: 42,
        });
        let request = a.originate_read(read, 1).unwrap();
        let onward = b.receive_state(request, 2, |_| panic!()).unwrap();
        let reply = c
            .receive_state(onward, 3, |_| StateReadResult::Busy)
            .unwrap();
        assert_eq!(reply.path_cost, TimePerByte::ZERO);
        let mut wire = [0; MAX_FRAME_SIZE];
        let size = reply.encode(1, &mut wire);
        let reply = b
            .receive_state(
                Message::decode(&wire[..size], 2, 15).unwrap(),
                4,
                |_| panic!(),
            )
            .unwrap();
        assert_eq!(reply.path_cost, TimePerByte::measured(2000, 100));
        let size = reply.encode(2, &mut wire);
        assert!(a
            .complete_read(Message::decode(&wire[..size], 1, 15).unwrap(), 5)
            .is_some());
        let cost = a.cost_to(3);
        assert_eq!(cost.estimate_us(10), Some(300));
        assert_eq!(cost.estimate_us(10_000), Some(300_000));
        assert_eq!(a.cost_to(1), TimePerByte::ZERO);
        assert_eq!(a.cost_to(99), TimePerByte::UNKNOWN);
        // Update only the first-hop measurement; retained downstream cost stays 20.
        a.record_transmission(
            Transmission {
                peer: 2,
                elapsed_us: 3000,
                acknowledged_bytes: 100,
            },
            6,
        );
        assert_eq!(a.cost_to(3).estimate_us(100), Some(4000));
        a.expire(ROUTE_MS + 10);
        assert_eq!(a.cost_to(3), TimePerByte::UNKNOWN);
    }

    #[test]
    fn measured_indirect_path_can_beat_a_maintained_direct_neighbor() {
        let mut nodes = [Forwarder::new(1, NET), Forwarder::new(9, NET)];
        connect(&mut nodes, 0, 1, 0);
        let node = &mut nodes[0];
        node.record_transmission(
            Transmission {
                peer: 9,
                elapsed_us: 9000,
                acknowledged_bytes: 100,
            },
            4,
        );
        node.record_transmission(
            Transmission {
                peer: 2,
                elapsed_us: 1000,
                acknowledged_bytes: 100,
            },
            4,
        );
        node.learn(9, 2, 3, 5, TimePerByte::measured(2000, 100));
        assert_eq!(node.next_hops(9, 1, 8), [2, 9]);
        assert_eq!(node.cost_to(9).estimate_us(100), Some(3000));
        // A known direct link still wins against an unmeasured suffix.
        node.learn(9, 2, 3, 6, TimePerByte::UNKNOWN);
        assert_eq!(node.next_hops(9, 1, 8), [9, 2]);
        // Same route ranking at either quantity: no special transfer class.
        let direct = node.cost_to(9);
        assert_eq!(
            direct.estimate_us(10_000).unwrap(),
            direct.estimate_us(10).unwrap() * 1000
        );
    }

    #[test]
    fn invocation_split_budget_and_partial_success_keep_the_surviving_direction() {
        let mut node = Forwarder::new(2, NET);
        node.learn(9, 3, 2, 0, TimePerByte::measured(4, 1));
        node.learn(9, 4, 2, 0, TimePerByte::measured(6, 1));
        let call = Invocation {
            caller: 1,
            owner: 9,
            request: 1,
            transformer: 42,
        };
        let request = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: Some(2),
            content: Content::Invoke {
                network: NET,
                call,
                remaining: 8,
                copies: 2,
                arguments: &[0],
            },
        };
        let sent = node.receive_control(request, 1, |_, _| panic!());
        assert_eq!(
            sent.map(|m| m.map(|m| m.destination)),
            [Some(Some(3)), Some(Some(4))]
        );
        for copy in sent.into_iter().flatten() {
            assert!(
                matches!(copy.content,Content::Invoke {call:c,copies:1,remaining:7,arguments:&[0],..} if c==call)
            );
        }
        let result = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 4,
            destination: Some(2),
            content: Content::InvocationResult {
                network: NET,
                call,
                status: InvokeStatus::Invoked,
                hops: 2,
            },
        };
        assert!(node.receive_control(result, 10, |_, _| panic!())[0].is_some());
        assert!(node
            .receive_control(result, 11, |_, _| panic!())
            .iter()
            .all(Option::is_none));
        node.expire(READ_MS + 1);
        assert_eq!(
            node.next_hops(9, 1, 8),
            [4, 0],
            "one successful copy masked the unanswered direction"
        );
        let mut downstream = Forwarder::new(4, NET);
        downstream.learn(9, 6, 2, 0, TimePerByte::measured(1, 1));
        downstream.learn(9, 7, 2, 0, TimePerByte::measured(2, 1));
        let one = downstream.receive_control(sent[1].unwrap(), 2, |_, _| panic!());
        assert!(
            one[0].is_some() && one[1].is_none(),
            "a descendant split again"
        );
        let mut empty = Forwarder::new(2, NET);
        let exploration = empty.receive_control(request, 0, |_, _| panic!());
        assert!(exploration[1].is_none());
        assert!(matches!(
            exploration[0].unwrap(),
            Message {
                destination: None,
                content: Content::Invoke { copies: 1, .. },
                ..
            }
        ));
        // No split is consumed while there is only one known direction.
        empty.learn(9, 3, 2, 0, TimePerByte::measured(4, 1));
        let next = Message {
            content: Content::Invoke {
                network: NET,
                call: Invocation { request: 2, ..call },
                remaining: 8,
                copies: 2,
                arguments: &[0],
            },
            ..request
        };
        assert!(matches!(
            empty.receive_control(next, 1, |_, _| panic!())[0]
                .unwrap()
                .content,
            Content::Invoke { copies: 2, .. }
        ));
    }

    #[test]
    fn link_walk_discovers_beyond_radio_range_and_teaches_surface_read_directions() {
        let mut nodes = [
            Forwarder::new(2, NET),
            Forwarder::new(3, NET),
            Forwarder::new(4, NET),
            Forwarder::new(5, NET),
        ];
        connect(&mut nodes, 0, 1, 0);
        connect(&mut nodes, 1, 2, 10);
        connect(&mut nodes, 2, 3, 20);
        let mut token = 1;
        for (owner, via, after, expected, hops) in [
            (2, 0, 0, Some(3), 1),
            (3, 2, 2, Some(4), 2),
            (4, 3, 3, Some(5), 3),
            (5, 4, 4, None, 4),
        ] {
            let read = LinkRead {
                reader: 1,
                owner,
                request: token,
                after,
                via,
            };
            token += 1;
            let query = Message {
                path_cost: TimePerByte::UNKNOWN,
                source: 1,
                destination: Some(2),
                content: Content::ReadLinks {
                    network: NET,
                    read,
                    remaining: 8,
                },
            };
            let result = walk(&mut nodes, query, 100);
            let Content::LinkEntry {
                link,
                hops: distance,
                ..
            } = result.content
            else {
                panic!()
            };
            assert_eq!(link.map(|l| l.peer), expected);
            assert_eq!(distance, hops);
        }
        // Walking established a path to owner 5 without a FindOwners flood.
        let description = Description::new(&SURFACE).unwrap();
        let read = SurfaceRead {
            reader: 1,
            owner: 5,
            request: token,
            offset: 0,
        };
        let query = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: Some(2),
            content: Content::ReadSurface {
                network: NET,
                read,
                remaining: 8,
            },
        };
        let mut wire = [0; MAX_FRAME_SIZE];
        let mut n = query.encode(0, &mut wire);
        for index in [0, 1, 2, 3, 2, 1, 0] {
            let local = nodes[index].local;
            let incoming = Message::decode(&wire[..n], local, 15).unwrap();
            let mut piece = [0; SURFACE_BYTES];
            let outgoing = nodes[index]
                .receive_surface(incoming, &description, &mut piece, 101)
                .unwrap();
            let target = outgoing
                .destination
                .expect("surface read lost learned direction");
            assert_eq!(target.abs_diff(local), 1);
            let mut next = [0; MAX_FRAME_SIZE];
            n = outgoing.encode(0, &mut next);
            wire = next;
        }
        let answer = Message::decode(&wire[..n], 1, 15).unwrap();
        assert!(matches!(answer.content,Content::SurfacePart {read:r,hops:4,..} if r==read));
        // A local route-cache replacement cannot erase the maintained neighbor.
        for owner in 90..95 {
            nodes[0].learn(owner, 3, 2, 102, TimePerByte::measured(1, 1));
        }
        let read = LinkRead {
            reader: 1,
            owner: 2,
            request: 99,
            after: 0,
            via: 0,
        };
        let reply = walk(
            &mut nodes,
            Message {
                path_cost: TimePerByte::UNKNOWN,
                source: 1,
                destination: Some(2),
                content: Content::ReadLinks {
                    network: NET,
                    read,
                    remaining: 8,
                },
            },
            103,
        );
        assert!(matches!(reply.content,Content::LinkEntry {link:Some(link),..} if link.peer==3));
    }

    #[test]
    fn guided_reads_reject_stale_links_and_correlate_via_and_cursor() {
        let mut nodes = [Forwarder::new(2, NET), Forwarder::new(3, NET)];
        connect(&mut nodes, 0, 1, 0);
        let read = LinkRead {
            reader: 1,
            owner: 3,
            request: 7,
            after: 0,
            via: 2,
        };
        let query = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: Some(2),
            content: Content::ReadLinks {
                network: NET,
                read,
                remaining: 8,
            },
        };
        let forwarded = nodes[0].receive_links(query, 10).unwrap();
        assert_eq!(forwarded.destination, Some(3));
        assert!(nodes[0].receive_links(query, 11).is_none());
        let actual = nodes[1].receive_links(forwarded, 12).unwrap();
        for wrong in [
            LinkRead { via: 0, ..read },
            LinkRead { after: 99, ..read },
            LinkRead { owner: 4, ..read },
        ] {
            let bad = Message {
                path_cost: TimePerByte::UNKNOWN,
                source: 3,
                destination: Some(2),
                content: Content::LinkEntry {
                    network: NET,
                    read: wrong,
                    revision: 1,
                    link: None,
                    hops: 1,
                },
            };
            assert!(nodes[0].receive_links(bad, 13).is_none());
        }
        assert!(nodes[0].receive_links(actual, 14).is_some());
        let foreign = Message {
            content: Content::ReadLinks {
                network: Network { founder: 99, ..NET },
                read,
                remaining: 8,
            },
            ..query
        };
        assert!(nodes[0].receive_links(foreign, 15).is_none());
        let exhausted = Message {
            content: Content::ReadLinks {
                network: NET,
                read: LinkRead { request: 8, ..read },
                remaining: 1,
            },
            ..query
        };
        assert!(nodes[0].receive_links(exhausted, 16).is_none());
        let stale = Message {
            content: Content::ReadLinks {
                network: NET,
                read: LinkRead { request: 9, ..read },
                remaining: 8,
            },
            ..query
        };
        assert!(
            nodes[0].receive_links(stale, 60_003).is_none(),
            "expired advertised link became a flood"
        );
        let local = Message {
            content: Content::ReadLinks {
                network: NET,
                read: LinkRead {
                    reader: 1,
                    owner: 2,
                    request: 10,
                    after: 0,
                    via: 0,
                },
                remaining: 1,
            },
            ..query
        };
        assert!(matches!(
            nodes[0].receive_links(local, 60_004).unwrap().content,
            Content::LinkEntry {
                revision: 2,
                link: None,
                ..
            }
        ));
        // Failed routing toward a via removes that direction, not a target route
        // which this request had not yet established.
        let mut relay = Forwarder::new(2, NET);
        relay.learn(4, 3, 2, 0, TimePerByte::measured(1, 1));
        let q = Message {
            content: Content::ReadLinks {
                network: NET,
                read: LinkRead {
                    owner: 5,
                    via: 4,
                    request: 11,
                    ..read
                },
                remaining: 8,
            },
            ..query
        };
        let sent = relay.receive_links(q, 1).unwrap();
        assert_eq!(sent.destination, Some(3));
        relay.transmit_failed(&sent);
        assert!(relay.routes.iter().all(|r| r.owner != 4));
        // Discovery content from another network or a relayed/broadcast exchange
        // cannot establish a maintained link.
        let offer = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 3,
            destination: None,
            content: Content::LinkExchange {
                network: NET,
                phase: LinkPhase::Offer,
                token: 1,
            },
        };
        assert!(relay.receive_link(offer, 2).is_none());
    }

    #[test]
    fn state_and_surface_reads_share_paths_without_accepting_mismatched_replies() {
        let mut relay = Forwarder::new(2, NET);
        let read = StateRead {
            reader: 1,
            owner: 4,
            request: 7,
            attribute: 42,
        };
        let query = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: None,
            content: Content::ReadState {
                network: NET,
                read,
                remaining: 8,
            },
        };
        let reply = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 3,
            destination: Some(2),
            content: Content::StateValue {
                network: NET,
                read,
                result: StateReadResult::Busy,
                hops: 2,
            },
        };
        assert!(relay.receive_state(reply, 0, |_| panic!()).is_none());
        assert_eq!(
            relay
                .receive_state(query, 0, |_| panic!())
                .unwrap()
                .destination,
            None
        );
        assert!(
            relay
                .receive_state(Message { source: 3, ..query }, 1, |_| panic!())
                .is_none(),
            "cycle forwarded duplicate read"
        );
        for wrong in [
            StateRead {
                attribute: 43,
                ..read
            },
            StateRead { owner: 5, ..read },
            StateRead { reader: 6, ..read },
            StateRead { request: 8, ..read },
        ] {
            let bad = Message {
                content: Content::StateValue {
                    network: NET,
                    read: wrong,
                    result: StateReadResult::Busy,
                    hops: 2,
                },
                ..reply
            };
            assert!(relay.receive_state(bad, 2, |_| panic!()).is_none());
        }
        let description = Description::new(&SURFACE).unwrap();
        let mut part = [0; SURFACE_BYTES];
        let surface_read = SurfaceRead {
            reader: 1,
            owner: 4,
            request: 7,
            offset: 0,
        };
        let surface_query = Message {
            content: Content::ReadSurface {
                network: NET,
                read: surface_read,
                remaining: 8,
            },
            ..query
        };
        assert!(
            relay
                .receive_surface(surface_query, &description, &mut part, 2)
                .is_none(),
            "different read kind reused a live token"
        );
        let surface_reply = Message {
            content: Content::SurfacePart {
                network: NET,
                read: surface_read,
                total: 13,
                hops: 2,
                bytes: &[0; 13],
            },
            ..reply
        };
        assert!(
            relay
                .receive_surface(surface_reply, &description, &mut part, 2)
                .is_none(),
            "surface bytes answered a state read"
        );
        let answer = relay.receive_state(reply, 3, |_| panic!()).unwrap();
        assert!(matches!(
            answer.content,
            Content::StateValue {
                result: StateReadResult::Busy,
                hops: 3,
                ..
            }
        ));
        assert_eq!(answer.destination, Some(1));
        assert!(relay.receive_state(reply, 4, |_| panic!()).is_none());
        let next = Message {
            content: Content::ReadState {
                network: NET,
                read: StateRead { request: 8, ..read },
                remaining: 8,
            },
            ..query
        };
        let directed = relay.receive_state(next, 5, |_| panic!()).unwrap();
        assert_eq!(directed.destination, Some(3));
        let unknown = Message {
            source: 5,
            content: Content::StateValue {
                network: NET,
                read: StateRead { request: 8, ..read },
                result: StateReadResult::UnknownAttribute,
                hops: 2,
            },
            ..reply
        };
        assert!(
            relay.receive_state(unknown, 6, |_| panic!()).is_none(),
            "unrelated neighbor settled directed read"
        );
        relay.expire(READ_MS + 5);
        assert!(
            relay
                .receive_state(reply, READ_MS + 6, |_| panic!())
                .is_none(),
            "expired result revived its path"
        );
        assert_eq!(
            relay
                .receive_state(next, READ_MS + 6, |_| panic!())
                .unwrap()
                .destination,
            None,
            "unanswered direction retained"
        );

        let mut owner = Forwarder::new(4, NET);
        let exhausted = Message {
            content: Content::ReadState {
                network: NET,
                read,
                remaining: 1,
            },
            ..query
        };
        assert!(Forwarder::new(2, NET)
            .receive_state(exhausted, 0, |_| panic!())
            .is_none());
        assert!(owner
            .receive_state(exhausted, 0, |attribute| {
                assert_eq!(attribute, 42);
                StateReadResult::UnknownAttribute
            })
            .is_some());
        assert!(owner
            .receive_state(exhausted, 1, |_| panic!("duplicate owner read"))
            .is_none());
        let foreign = Message {
            content: Content::ReadState {
                network: Network { founder: 77, ..NET },
                read,
                remaining: 8,
            },
            ..query
        };
        assert!(owner.receive_state(foreign, 2, |_| panic!()).is_none());
        // Surface traffic competes for exactly the same fixed read-path capacity.
        let mut mixed = Forwarder::new(2, NET);
        mixed.receive_state(query, 0, |_| panic!()).unwrap();
        for id in 8..8 + READ_PATHS as u16 {
            let q = Message {
                content: Content::ReadSurface {
                    network: NET,
                    read: SurfaceRead {
                        request: id,
                        ..surface_read
                    },
                    remaining: 8,
                },
                ..query
            };
            mixed
                .receive_surface(q, &description, &mut part, u32::from(id))
                .unwrap();
        }
        assert!(
            mixed.receive_state(reply, 20, |_| panic!()).is_none(),
            "evicted state path retained a shadow entry"
        );
    }

    #[test]
    fn control_paths_and_deltas_have_bounded_scope_and_expiring_history() {
        let network = Network {
            founder: 9,
            channel: 15,
        };
        let mut relay = Forwarder::new(2, network);
        let call = Invocation {
            caller: 1,
            owner: 4,
            request: 1,
            transformer: 42,
        };
        let request = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: Some(2),
            content: Content::Invoke {
                copies: 1,
                network,
                call,
                remaining: MAX_HOPS,
                arguments: &[0],
            },
        };
        let response = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 3,
            destination: Some(2),
            content: Content::InvocationResult {
                network,
                call,
                status: InvokeStatus::Invoked,
                hops: 2,
            },
        };
        assert!(relay.receive_control(response, 0, |_, _| panic!())[0].is_none());
        let out = relay.receive_control(request, 0, |_, _| panic!())[0].unwrap();
        assert_eq!(out.destination, None);
        assert!(matches!(out.content, Content::Invoke { remaining: 7, .. }));
        assert!(relay.receive_control(
            Message {
                source: 3,
                content: Content::Invoke {
                    network,
                    call,
                    remaining: 6,
                    copies: 1,
                    arguments: &[0]
                },
                ..request
            },
            1,
            |_, _| panic!()
        )[0]
        .is_none());
        assert!(relay.receive_control(
            Message {
                source: 1,
                ..response
            },
            2,
            |_, _| panic!()
        )[0]
        .is_none());
        let reply = relay.receive_control(response, 3, |_, _| panic!())[0].unwrap();
        assert!(matches!(
            reply.content,
            Content::InvocationResult { hops: 3, .. }
        ));
        let next = Message {
            content: Content::Invoke {
                copies: 1,
                network,
                call: Invocation { request: 2, ..call },
                remaining: MAX_HOPS,
                arguments: &[0],
            },
            ..request
        };
        let directed = relay.receive_control(next, 4, |_, _| panic!())[0].unwrap();
        assert_eq!(directed.destination, Some(3));
        // A reply from an unrelated neighbor cannot settle a directed request.
        let wrong = Message {
            source: 5,
            content: Content::InvocationResult {
                network,
                call: Invocation { request: 2, ..call },
                status: InvokeStatus::Invoked,
                hops: 2,
            },
            ..response
        };
        assert!(relay.receive_control(wrong, 5, |_, _| panic!())[0].is_none());
        relay.expire(READ_MS + 4);
        assert_eq!(
            relay.receive_control(next, READ_MS + 5, |_, _| panic!())[0]
                .unwrap()
                .destination,
            None
        );
        let exhausted = Message {
            content: Content::Invoke {
                copies: 1,
                network,
                call: Invocation { request: 3, ..call },
                remaining: 1,
                arguments: &[0],
            },
            ..request
        };
        assert!(relay.receive_control(exhausted, READ_MS + 6, |_, _| panic!())[0].is_none());
        let other = Network {
            founder: 10,
            ..network
        };
        assert!(relay.receive_control(
            Message {
                content: Content::Invoke {
                    copies: 1,
                    network: other,
                    call,
                    remaining: 8,
                    arguments: &[0]
                },
                ..request
            },
            READ_MS + 7,
            |_, _| panic!()
        )[0]
        .is_none());
        let change = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 4,
            destination: None,
            content: Content::StateChanged {
                network,
                delta: StateChange {
                    owner: 4,
                    attribute: 42,
                    sequence: 1,
                    value: None,
                },
                hops: MAX_HOPS,
            },
        };
        assert!(relay
            .receive_change(change, 0, |_| panic!("hop budget exhausted"))
            .is_some());
        assert!(relay.receive_change(change, 1, |_| panic!()).is_none());
        assert!(relay
            .receive_change(change, READ_MS, |_| panic!())
            .is_some());
        // Sequence identity belongs to the owner, not its value or incoming neighbor.
        let newer = Message {
            source: 3,
            content: Content::StateChanged {
                network,
                delta: StateChange {
                    owner: 4,
                    attribute: 42,
                    sequence: 2,
                    value: None,
                },
                hops: 2,
            },
            ..change
        };
        let mut sent = 0;
        assert!(relay
            .receive_change(newer, READ_MS + 1, |_| sent += 1)
            .is_some());
        assert_eq!(sent, 1);
        assert!(relay
            .receive_change(Message { source: 5, ..newer }, READ_MS + 2, |_| panic!())
            .is_none());
    }

    #[test]
    fn exploration_returns_ranges_and_learns_directions_through_two_relays() {
        let mut nodes = [
            Forwarder::new(2, NET),
            Forwarder::new(3, NET),
            Forwarder::new(4, NET),
        ];
        let links = [(1, 2), (2, 3), (3, 4)];
        let description = Description::new(&SURFACE).unwrap();
        let mut expected = self::std::vec![0; description.len() as usize];
        description.encode_part(0, &mut expected);
        let mut received = Vec::new();
        let mut request = 0;
        while received.len() < expected.len() {
            let (bytes, reads, _) = exchange(
                &mut nodes,
                &links,
                query(request, received.len() as u16),
                100 + u32::from(request),
            );
            if request == 0 {
                assert_eq!(reads, [(2, None), (3, None)]);
            } else {
                assert_eq!(reads, [(2, Some(3)), (3, Some(4))]);
            }
            assert!(!bytes.is_empty());
            received.extend_from_slice(&bytes);
            request += 1;
        }
        assert_eq!(received, expected);
        // A lost response can be recovered with a fresh request for the same range.
        assert_eq!(
            exchange(&mut nodes, &links, query(request, 0), 200).0,
            expected[..SURFACE_BYTES]
        );
    }

    #[test]
    fn duplicate_paths_and_cycles_do_not_amplify_one_read() {
        let mut nodes = [
            Forwarder::new(2, NET),
            Forwarder::new(3, NET),
            Forwarder::new(4, NET),
        ];
        let links = [(1, 2), (1, 3), (2, 3), (2, 4), (3, 4)];
        let (bytes, reads, _) = exchange(&mut nodes, &links, query(9, 0), 10);
        assert_eq!(bytes.len(), SURFACE_BYTES);
        assert_eq!(reads.len(), 2);
        assert!(exchange(&mut nodes, &links, query(9, 0), 11).0.is_empty());
        assert_eq!(exchange(&mut nodes, &links, query(10, 0), 12).0, bytes);
    }

    #[test]
    fn discovery_finds_multiple_owners_then_reads_beyond_direct_range() {
        let mut nodes = [
            Forwarder::new(1, NET),
            Forwarder::new(2, NET),
            Forwarder::new(3, NET),
            Forwarder::new(4, NET),
        ];
        let description = Description::new(&SURFACE).unwrap();
        let links = [(1, 2), (2, 3), (3, 4)];
        let find = nodes[0].find_owners(42, 100);
        let (bytes, reads, observed) = exchange(&mut nodes, &links, find, 101);
        assert!(
            bytes.is_empty() && reads.is_empty(),
            "discovery must not publish surfaces"
        );
        let mut owners: Vec<_> = observed
            .iter()
            .filter(|(local, _)| *local == 1)
            .map(|(_, o)| *o)
            .collect();
        owners.sort_by_key(|o| o.address);
        assert_eq!(
            owners.iter().map(|o| o.address).collect::<Vec<_>>(),
            [2, 3, 4]
        );
        assert!(owners.iter().all(|o| o.surface_bytes == description.len()));
        assert_eq!(nodes[0].next_hop(4, 0, MAX_HOPS).unwrap(), 2);
        // The known owner and directions came from discovery, not test setup.
        let mut read = query(43, 0);
        read.destination = Some(nodes[0].next_hop(4, 0, MAX_HOPS).unwrap());
        let (bytes, reads, _) = exchange(&mut nodes, &links, read, 102);
        assert_eq!(bytes.len(), SURFACE_BYTES);
        assert_eq!(reads, [(2, Some(3)), (3, Some(4))]);
    }

    #[test]
    fn arrivals_cross_relays_once_without_triggering_surface_or_discovery_responses() {
        let mut nodes = [
            Forwarder::new(1, NET),
            Forwarder::new(2, NET),
            Forwarder::new(3, NET),
            Forwarder::new(4, NET),
        ];
        let description = Description::new(&SURFACE).unwrap();
        let links = [(1, 2), (1, 3), (2, 3), (2, 4), (3, 4)];
        let arrival = nodes[3].announce(17, description.len(), 100);
        let (bytes, reads, observed) = exchange(&mut nodes, &links, arrival, 101);
        assert!(bytes.is_empty() && reads.is_empty());
        assert_eq!(observed.len(), 3);
        assert!(observed.iter().all(|(_, o)| o.address == 4));
        assert!(
            nodes.iter().all(|n| n.routes.iter().all(|r| r.owner == 0)),
            "an announcement is not proof of a return path"
        );
        assert!(exchange(&mut nodes, &links, arrival, 102).2.is_empty());
        let fresh = nodes[3].announce(18, description.len(), 103);
        assert_eq!(exchange(&mut nodes, &links, fresh, 104).2.len(), 3);
    }

    #[test]
    fn cyclic_discovery_suppresses_duplicates_per_owner_not_per_query() {
        let mut nodes = [
            Forwarder::new(1, NET),
            Forwarder::new(2, NET),
            Forwarder::new(3, NET),
            Forwarder::new(4, NET),
        ];
        let links = [(1, 2), (1, 3), (2, 3), (2, 4), (3, 4)];
        let find = nodes[0].find_owners(7, 1);
        let owners = exchange(&mut nodes, &links, find, 2).2;
        assert_eq!(owners.iter().filter(|(local, _)| *local == 1).count(), 3);
        assert!(exchange(&mut nodes, &links, find, 3).2.is_empty());
        let find = nodes[0].find_owners(8, 4);
        assert_eq!(
            exchange(&mut nodes, &links, find, 5)
                .2
                .iter()
                .filter(|(local, _)| *local == 1)
                .count(),
            3
        );
    }

    #[test]
    fn owner_replies_require_a_live_query_and_history_has_bounded_eviction() {
        let mut node = Forwarder::new(1, NET);
        let owner = SurfaceOwner {
            address: 4,
            surface_bytes: 195,
        };
        let found = |request| Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 2,
            destination: Some(1),
            content: Content::OwnerFound {
                network: NET,
                seeker: 1,
                request,
                owner,
                hops: 3,
            },
        };
        assert!(node
            .receive_discovery(found(0), 13, 0, |_| panic!())
            .is_none());
        for request in 0..=DISCOVERY_PATHS as u16 {
            node.find_owners(request, 1 + u32::from(request));
        }
        assert!(node
            .receive_discovery(found(0), 13, 10, |_| panic!())
            .is_none());
        assert!(node.routes.iter().all(|r| r.owner == 0));
        assert_eq!(
            node.receive_discovery(found(DISCOVERY_PATHS as u16), 13, 10, |_| panic!()),
            Some(owner)
        );
        assert!(node
            .receive_discovery(found(DISCOVERY_PATHS as u16), 13, 11, |_| panic!())
            .is_none());
        node.expire(READ_MS + 10);
        assert!(node
            .receive_discovery(
                found(DISCOVERY_PATHS as u16),
                13,
                READ_MS + 11,
                |_| panic!()
            )
            .is_none());
        // Announcement duplicate history is bounded independently of read paths.
        for i in 0..=OWNER_NOTICES as u16 {
            assert!(node.notice(10 + u64::from(i), 0, i, 20 + u32::from(i)));
        }
        assert!(node.notice(10, 0, 0, 100));
        assert!(!node.notice(10, 0, 0, 101));
        let mut wrap = Forwarder::new(1, NET);
        wrap.find_owners(99, u32::MAX - 100);
        wrap.expire(READ_MS);
        assert!(wrap.discovery.iter().all(|p| p.seeker == 0));
    }

    #[test]
    fn discovery_scope_and_foreign_networks_do_not_leak_across_boundaries() {
        let mut node = Forwarder::new(2, NET);
        let mut sent = Vec::new();
        let find = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: None,
            content: Content::FindOwners {
                network: NET,
                seeker: 1,
                request: 1,
                remaining: 1,
            },
        };
        node.receive_discovery(find, 13, 0, |m| sent.push(m));
        assert_eq!(sent.len(), 1);
        assert!(matches!(
            sent[0].content,
            Content::OwnerFound {
                owner: SurfaceOwner { address: 2, .. },
                ..
            }
        ));
        assert_eq!(sent[0].destination, Some(1));
        let mut foreign = find;
        if let Content::FindOwners {
            ref mut network,
            ref mut request,
            ..
        } = foreign.content
        {
            network.founder = 99;
            *request = 2;
        }
        assert!(node
            .receive_discovery(foreign, 13, 1, |_| panic!())
            .is_none());
        let owner = SurfaceOwner {
            address: 4,
            surface_bytes: 13,
        };
        let arrival = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 3,
            destination: None,
            content: Content::Arrive {
                network: NET,
                owner,
                sequence: 7,
                hops: MAX_HOPS,
            },
        };
        assert_eq!(
            node.receive_discovery(arrival, 13, 2, |_| panic!()),
            Some(owner)
        );
        // An owner notice can end at the hop boundary, but cannot be relayed beyond it.
        let found = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 3,
            destination: Some(2),
            content: Content::OwnerFound {
                network: NET,
                seeker: 1,
                request: 1,
                owner,
                hops: MAX_HOPS,
            },
        };
        assert!(node.receive_discovery(found, 13, 3, |_| panic!()).is_none());
        let mut origin = Forwarder::new(1, NET);
        origin.find_owners(1, 0);
        assert_eq!(
            origin.receive_discovery(
                Message {
                    destination: Some(1),
                    ..found
                },
                13,
                3,
                |_| panic!()
            ),
            Some(owner)
        );
    }

    #[test]
    fn replies_require_the_original_read_and_unmatched_data_cannot_teach_routes() {
        let description = Description::new(&SURFACE).unwrap();
        let mut node = Forwarder::new(2, NET);
        let mut piece = [0; SURFACE_BYTES];
        assert!(node
            .receive_surface(reply(1, 0), &description, &mut piece, 0)
            .is_none());
        node.receive_surface(query(1, 0), &description, &mut piece, 1)
            .unwrap();
        for (request, offset) in [(2, 0), (1, 1)] {
            assert!(node
                .receive_surface(reply(request, offset), &description, &mut piece, 2)
                .is_none());
        }
        assert!(node.routes.iter().all(|r| r.owner == 0));
        let out = node
            .receive_surface(reply(1, 0), &description, &mut piece, 3)
            .unwrap();
        assert_eq!(out.destination, Some(1));
        assert!(node
            .receive_surface(reply(1, 0), &description, &mut piece, 4)
            .is_none());
        assert_eq!(node.routes.iter().filter(|r| r.owner != 0).count(), 1);
        let mut bad = query(3, 0);
        if let Content::ReadSurface {
            ref mut network, ..
        } = bad.content
        {
            network.founder = 99;
        }
        assert!(node
            .receive_surface(bad, &description, &mut piece, 5)
            .is_none());
    }

    #[test]
    fn hop_limits_owner_bounds_and_read_path_eviction_are_local() {
        assert!(core::mem::size_of::<Forwarder>() <= 2464);
        let description = Description::new(&SURFACE).unwrap();
        let mut relay = Forwarder::new(2, NET);
        let mut owner = Forwarder::new(4, NET);
        let mut piece = [0; SURFACE_BYTES];
        let mut last = query(1, 0);
        if let Content::ReadSurface {
            ref mut remaining, ..
        } = last.content
        {
            *remaining = 1;
        }
        assert!(relay
            .receive_surface(last, &description, &mut piece, 0)
            .is_none());
        assert!(owner
            .receive_surface(last, &description, &mut piece, 0)
            .is_some());
        assert!(owner
            .receive_surface(query(2, description.len() + 1), &description, &mut piece, 0)
            .is_none());
        let eof = owner
            .receive_surface(query(3, description.len()), &description, &mut piece, 0)
            .unwrap();
        assert!(matches!(
            eof.content,
            Content::SurfacePart { bytes: [], .. }
        ));
        for request in 0..=READ_PATHS as u16 {
            relay.receive_surface(
                query(request, 0),
                &description,
                &mut piece,
                u32::from(request),
            );
        }
        assert!(relay
            .receive_surface(reply(0, 0), &description, &mut piece, 100)
            .is_none());
        assert!(relay
            .receive_surface(reply(READ_PATHS as u16, 0), &description, &mut piece, 100)
            .is_some());
    }

    #[test]
    fn failure_and_expiry_restore_exploration_without_erasing_new_success() {
        let description = Description::new(&SURFACE).unwrap();
        let mut node = Forwarder::new(2, NET);
        let mut piece = [0; SURFACE_BYTES];
        node.learn(4, 3, 2, 0, TimePerByte::measured(1, 1));
        let out = node
            .receive_surface(query(1, 0), &description, &mut piece, 1)
            .unwrap();
        assert_eq!(out.destination, Some(3));
        node.transmit_failed(&out);
        assert_eq!(
            node.receive_surface(query(2, 0), &description, &mut piece, 2)
                .unwrap()
                .destination,
            None
        );
        node.receive_surface(reply(2, 0), &description, &mut piece, 3)
            .unwrap();
        node.receive_surface(query(3, 0), &description, &mut piece, 4)
            .unwrap(); // no response
        node.expire(READ_MS + 4);
        assert_eq!(
            node.receive_surface(query(4, 0), &description, &mut piece, READ_MS + 5)
                .unwrap()
                .destination,
            None
        );
        node.receive_surface(reply(4, 0), &description, &mut piece, READ_MS + 6)
            .unwrap();
        node.receive_surface(query(5, 0), &description, &mut piece, READ_MS + 7)
            .unwrap();
        node.receive_surface(query(6, 0), &description, &mut piece, READ_MS + 8)
            .unwrap();
        node.receive_surface(reply(6, 0), &description, &mut piece, READ_MS + 9)
            .unwrap();
        node.expire(2 * READ_MS + 7);
        assert_eq!(
            node.receive_surface(query(7, 0), &description, &mut piece, 2 * READ_MS + 8)
                .unwrap()
                .destination,
            Some(3)
        );
        let mut wrap = Forwarder::new(2, NET);
        wrap.learn(4, 3, 2, u32::MAX - 100, TimePerByte::measured(1, 1));
        wrap.receive_surface(query(1, 0), &description, &mut piece, u32::MAX - 99)
            .unwrap();
        wrap.expire(READ_MS);
        assert_eq!(
            wrap.receive_surface(query(2, 0), &description, &mut piece, READ_MS + 1)
                .unwrap()
                .destination,
            None
        );
        for (i, owner) in (4..8).enumerate() {
            node.learn(owner, 3, 2, 100 + i as u32, TimePerByte::measured(1, 1));
        }
        assert!(node.routes.iter().all(|r| r.owner != 4));
        node.expire(ROUTE_MS + 104);
        assert!(node.routes.iter().all(|r| r.owner == 0));
    }
}
