//! Bounded discovery, owner-directed reads/calls, and state-delta forwarding.
//! Return paths and duplicate records are communication state, never semantic
//! edges or queued graph activations.

use crate::kernel::radio::cost::{TimePerByte, Transmission};

use super::{
    exposure::Description,
    frame::{
        Content, Invocation, InvokeStatus, LinkPhase, LinkRead, Message, ProgramRead, StateChange,
        StateRead, StateReadResult, SurfaceOwner, SurfaceRead, UpdateRead, CALL_ARGUMENT_BYTES,
        MAX_HOPS, SURFACE_BYTES,
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
    Program(ProgramRead),
    Update(UpdateRead),
    Surface(SurfaceRead),
    State(StateRead),
    Links(LinkRead),
}
impl ReadRequest {
    pub(super) fn reader(self) -> u64 {
        match self {
            Self::Program(r) => r.reader,
            Self::Update(r) => r.reader,
            Self::Surface(r) => r.reader,
            Self::State(r) => r.reader,
            Self::Links(r) => r.reader,
        }
    }
    pub(super) fn owner(self) -> u64 {
        match self {
            Self::Program(r) => r.owner,
            Self::Update(r) => r.owner,
            Self::Surface(r) => r.owner,
            Self::State(r) => r.owner,
            Self::Links(r) => r.owner,
        }
    }
    fn request(self) -> u16 {
        match self {
            Self::Program(r) => r.request,
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
    pub(super) fn ordinary_work_pending(&self) -> bool {
        self.reads.iter().any(|r| {
            r.read.reader() != 0
                && !r.answered
                && !matches!(r.read, ReadRequest::Update(_) | ReadRequest::Program(_))
        }) || self
            .calls
            .iter()
            .any(|c| c.call.caller != 0 && c.result.is_none())
    }
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
            | Content::ProgramResult { hops, .. }
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
        if matches!(request, ReadRequest::Update(_) | ReadRequest::Program(_)) {
            return None;
        }
        let destination = self.originate_read_path(request, now)?;
        Some(Message {
            path_cost: TimePerByte::UNKNOWN,
            source: self.local,
            destination,
            content: match request {
                ReadRequest::Update(_) | ReadRequest::Program(_) => return None, // payload-bearing origins
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
    pub(super) fn originate_program<'a>(
        &mut self,
        read: ProgramRead,
        bytes: &'a [u8],
        now: u32,
    ) -> Option<Message<'a>> {
        let destination = self.originate_read_path(ReadRequest::Program(read), now)?;
        Some(Message {
            source: self.local,
            destination,
            path_cost: TimePerByte::UNKNOWN,
            content: Content::Program {
                network: self.network,
                read,
                remaining: MAX_HOPS,
                bytes,
            },
        })
    }
    pub(super) fn receive_program<'a>(
        &mut self,
        incoming: Message<'a>,
        now: u32,
        output: &'a mut [u8; lattice_peer_wire::program::transfer::REPLY_BYTES],
        mut handle: impl FnMut(
            u64,
            lattice_peer_wire::program::transfer::Request<'_>,
            &mut [u8; lattice_peer_wire::program::transfer::REPLY_BYTES],
        ) -> usize,
    ) -> Option<Message<'a>> {
        self.expire(now);
        match incoming.content {
            Content::Program {
                network,
                read,
                remaining,
                bytes,
            } if network == self.network => {
                let next = self
                    .begin_read(ReadRequest::Program(read), incoming.source, remaining, now)?
                    .onward;
                if read.owner == self.local {
                    let request = lattice_peer_wire::program::transfer::Request::decode(bytes)?;
                    let n = handle(read.reader, request, output);
                    return Some(Message {
                        source: self.local,
                        destination: Some(incoming.source),
                        path_cost: TimePerByte::ZERO,
                        content: Content::ProgramResult {
                            network,
                            read,
                            hops: 1,
                            bytes: &output[..n],
                        },
                    });
                }
                Some(Message {
                    source: self.local,
                    destination: (next != 0).then_some(next),
                    path_cost: TimePerByte::UNKNOWN,
                    content: Content::Program {
                        network,
                        read,
                        remaining: remaining - 1,
                        bytes,
                    },
                })
            }
            Content::ProgramResult {
                network,
                read,
                hops,
                bytes,
            } if network == self.network && incoming.destination == Some(self.local) => {
                let previous = self.return_read(
                    ReadRequest::Program(read),
                    incoming.source,
                    hops,
                    now,
                    incoming.path_cost,
                )?;
                Some(Message {
                    source: self.local,
                    destination: Some(previous),
                    path_cost: self.response_cost(incoming),
                    content: Content::ProgramResult {
                        network,
                        read,
                        hops: hops + 1,
                        bytes,
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
            Content::ProgramResult {
                network,
                read,
                hops,
                ..
            } => (network, ReadRequest::Program(read), hops),
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
        invoke: impl FnOnce(Invocation, &[u8]) -> Option<InvokeStatus>,
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
                    let Some(status) = invoke(call, arguments) else {
                        return [None; 2];
                    };
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
            Content::Program { read, .. } => read.owner,
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
    use lattice_peer_wire::update::Progress;
    const NET: Network = Network {
        founder: 1,
        channel: 15,
    };
    const CALL: Invocation = Invocation {
        caller: 1,
        owner: 4,
        request: 1,
        transformer: 42,
    };
    const STATUS: [u8; 17] = {
        let mut b = [9; 17];
        b[0] = 4;
        b
    };
    fn reads(reader: u64, owner: u64, request: u16) -> [ReadRequest; 5] {
        [
            ReadRequest::Program(ProgramRead {
                reader,
                owner,
                request,
            }),
            ReadRequest::Surface(SurfaceRead {
                reader,
                owner,
                request,
                offset: 0,
            }),
            ReadRequest::State(StateRead {
                reader,
                owner,
                request,
                attribute: 42,
            }),
            ReadRequest::Links(LinkRead {
                reader,
                owner,
                request,
                after: 0,
                via: 0,
            }),
            ReadRequest::Update(UpdateRead {
                reader,
                owner,
                request,
                package: [9; 16],
            }),
        ]
    }
    fn envelope(content: Content<'static>, source: u64) -> Message<'static> {
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source,
            destination: Some(2),
            content,
        }
    }
    fn query(read: ReadRequest, network: Network, remaining: u8) -> Message<'static> {
        envelope(
            match read {
                ReadRequest::Program(read) => Content::Program {
                    network,
                    read,
                    remaining,
                    bytes: &[0],
                },
                ReadRequest::Surface(read) => Content::ReadSurface {
                    network,
                    read,
                    remaining,
                },
                ReadRequest::State(read) => Content::ReadState {
                    network,
                    read,
                    remaining,
                },
                ReadRequest::Links(read) => Content::ReadLinks {
                    network,
                    read,
                    remaining,
                },
                ReadRequest::Update(read) => Content::Update {
                    network,
                    read,
                    remaining,
                    bytes: &STATUS,
                },
            },
            1,
        )
    }
    fn answer(read: ReadRequest, hops: u8) -> Message<'static> {
        envelope(
            match read {
                ReadRequest::Program(read) => Content::ProgramResult {
                    network: NET,
                    read,
                    hops,
                    bytes: &[0; 12],
                },
                ReadRequest::Surface(read) => Content::SurfacePart {
                    network: NET,
                    read,
                    total: 13,
                    bytes: &[0; 13],
                    hops,
                },
                ReadRequest::State(read) => Content::StateValue {
                    network: NET,
                    read,
                    result: StateReadResult::Value {
                        sequence: 7,
                        value: Some(crate::ir::Value::boolean(false)),
                    },
                    hops,
                },
                ReadRequest::Links(read) => Content::LinkEntry {
                    network: NET,
                    read,
                    revision: 7,
                    link: None,
                    hops,
                },
                ReadRequest::Update(read) => Content::Updated {
                    network: NET,
                    read,
                    progress: Progress::unsupported(read.package),
                    hops,
                },
            },
            3,
        )
    }
    // One handler call, no simulated peers, packet scheduler, or codec round trip.
    fn receive<'a>(
        node: &mut Forwarder,
        input: Message<'static>,
        part: &'a mut [u8; SURFACE_BYTES],
        now: u32,
    ) -> Option<Message<'a>> {
        let local = node.local;
        match input.content {
            Content::Program { .. } | Content::ProgramResult { .. } => node.receive_program(
                input,
                now,
                (&mut part[..lattice_peer_wire::program::transfer::REPLY_BYTES])
                    .try_into()
                    .unwrap(),
                |_, _, output| {
                    assert_eq!(local, 4);
                    lattice_peer_wire::program::transfer::Reply::status(
                        lattice_peer_wire::program::transfer::Status::Unsupported,
                    )
                    .encode(output)
                },
            ),
            Content::ReadSurface { .. } | Content::SurfacePart { .. } => node.receive_surface(
                input,
                &Description::new(&crate::surface::EMPTY).unwrap(),
                part,
                now,
            ),
            Content::ReadState { .. } | Content::StateValue { .. } => {
                node.receive_state(input, now, |_| {
                    assert_eq!(local, 4);
                    StateReadResult::Busy
                })
            }
            Content::ReadLinks { .. } | Content::LinkEntry { .. } => node.receive_links(input, now),
            Content::Update { .. } | Content::Updated { .. } => {
                node.receive_update(input, now, |r| {
                    assert_eq!(local, 4);
                    Progress::unsupported(r.package())
                })
            }
            _ => unreachable!(),
        }
    }
    fn link(node: &mut Forwarder, peer: u64) {
        node.links.receive(peer, LinkPhase::Offer, 1, 0);
        node.links.receive(peer, LinkPhase::Confirm, 1, 1);
        assert!(node.links.contains(peer));
    }
    fn cost(node: &mut Forwarder, peer: u64, unit: u32) {
        node.record_transmission(
            Transmission {
                peer,
                elapsed_us: unit * 100,
                acknowledged_bytes: 100,
            },
            0,
        );
    }

    #[test]
    fn read_admission() {
        for reader in [1, 2] {
            for owner in [2, 4] {
                for remaining in [1, 2, MAX_HOPS] {
                    for read in reads(reader, owner, 7) {
                        let mut node = Forwarder::new(2, NET);
                        let allowed = reader != 2 && (owner == 2 || remaining > 1);
                        assert_eq!(node.begin_read(read, 1, remaining, 0).is_some(), allowed);
                        if allowed {
                            for duplicate in reads(reader, owner, 7) {
                                assert!(node.begin_read(duplicate, 3, MAX_HOPS, 1).is_none());
                            }
                            assert_eq!(
                                node.reads
                                    .iter()
                                    .find(|r| r.read.reader() != 0)
                                    .unwrap()
                                    .previous,
                                1
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn read_correlation() {
        for read in reads(1, 4, 7) {
            let mut wrong = Vec::from(reads(1, 4, 7));
            wrong.extend(reads(9, 4, 7));
            wrong.extend(reads(1, 9, 7));
            wrong.extend(reads(1, 4, 8));
            wrong.extend([
                ReadRequest::Surface(SurfaceRead {
                    reader: 1,
                    owner: 4,
                    request: 7,
                    offset: 1,
                }),
                ReadRequest::State(StateRead {
                    reader: 1,
                    owner: 4,
                    request: 7,
                    attribute: 43,
                }),
                ReadRequest::Links(LinkRead {
                    reader: 1,
                    owner: 4,
                    request: 7,
                    after: 1,
                    via: 0,
                }),
                ReadRequest::Links(LinkRead {
                    reader: 1,
                    owner: 4,
                    request: 7,
                    after: 0,
                    via: 3,
                }),
                ReadRequest::Update(UpdateRead {
                    reader: 1,
                    owner: 4,
                    request: 7,
                    package: [8; 16],
                }),
            ]);
            for candidate in wrong.into_iter().filter(|r| *r != read) {
                let mut node = Forwarder::new(2, NET);
                node.begin_read(read, 1, MAX_HOPS, 0).unwrap();
                assert_eq!(
                    node.return_read(candidate, 3, 2, 1, TimePerByte::UNKNOWN),
                    None
                );
                assert!(node.routes.iter().all(|r| r.owner == 0));
            }
            for (onward, source, hops, allowed) in [
                (0, 1, 2, false),
                (0, 3, MAX_HOPS, false),
                (0, 3, MAX_HOPS - 1, true),
                (3, 5, 2, false),
                (3, 3, 2, true),
            ] {
                let mut node = Forwarder::new(2, NET);
                node.remember_read(read, 1, onward, false, 0);
                assert_eq!(
                    node.return_read(read, source, hops, 1, TimePerByte::UNKNOWN),
                    allowed.then_some(1)
                );
                if allowed {
                    assert_eq!(
                        node.return_read(read, source, hops, 2, TimePerByte::UNKNOWN),
                        None
                    );
                }
                assert_eq!(node.routes.iter().any(|r| r.owner == 4), allowed);
            }
        }
    }

    #[test]
    fn read_envelopes() {
        for read in reads(1, 4, 7) {
            for network in [NET, Network { founder: 99, ..NET }] {
                for local in [2, 4] {
                    for remaining in [1, 2] {
                        let mut node = Forwarder::new(local, NET);
                        let mut part = [0; SURFACE_BYTES];
                        let input = Message {
                            destination: Some(local),
                            ..query(read, network, remaining)
                        };
                        let out = receive(&mut node, input, &mut part, 0);
                        let allowed = network == NET && (local == 4 || remaining > 1);
                        assert_eq!(out.is_some(), allowed);
                        if let Some(out) = out {
                            assert_eq!(out.destination, (local == 4).then_some(1));
                        }
                        assert!(receive(&mut node, input, &mut part, 1).is_none());
                    }
                }
            }
            let mut node = Forwarder::new(2, NET);
            let mut part = [0; SURFACE_BYTES];
            cost(&mut node, 3, 10);
            receive(&mut node, query(read, NET, 8), &mut part, 0).unwrap();
            let incoming = Message {
                path_cost: TimePerByte::measured(20, 1),
                ..answer(read, 2)
            };
            let out = receive(&mut node, incoming, &mut part, 1).unwrap();
            assert_eq!(out.destination, Some(1));
            assert_eq!(out.content, answer(read, 3).content);
            assert_eq!(out.path_cost, TimePerByte::measured(30, 1));
            assert_eq!(node.cost_to(4), out.path_cost);
            cost(&mut node, 3, 5);
            assert_eq!(node.cost_to(4), TimePerByte::measured(55, 2));
        }
        let description = Description::new(&crate::surface::EMPTY).unwrap();
        for offset in [
            description.len() - 1,
            description.len(),
            description.len() + 1,
        ] {
            let mut node = Forwarder::new(4, NET);
            let mut part = [0; SURFACE_BYTES];
            let input = query(
                ReadRequest::Surface(SurfaceRead {
                    reader: 1,
                    owner: 4,
                    request: 7,
                    offset,
                }),
                NET,
                1,
            );
            let out = receive(&mut node, input, &mut part, 0);
            assert_eq!(out.is_some(), offset <= description.len());
            if let Some(Message {
                content: Content::SurfacePart { bytes, .. },
                ..
            }) = out
            {
                assert_eq!(bytes.len(), usize::from(description.len() - offset));
            }
        }
    }

    #[test]
    fn guided_links() {
        for linked in [false, true] {
            for via in [0, 2, 3] {
                let mut node = Forwarder::new(2, NET);
                if linked {
                    link(&mut node, 4);
                }
                node.learn(3, 5, 2, 1, TimePerByte::UNKNOWN);
                let read = ReadRequest::Links(LinkRead {
                    reader: 1,
                    owner: 4,
                    request: 1,
                    via,
                    after: 0,
                });
                let out = node.receive_links(query(read, NET, 8), 2);
                assert_eq!(out.is_some(), via != 2 || linked);
                if let Some(out) = out {
                    assert_eq!(
                        out.destination,
                        if via == 3 {
                            Some(5)
                        } else {
                            linked.then_some(4)
                        }
                    );
                    if via == 3 {
                        node.transmit_failed(&out);
                        assert!(node.routes.iter().all(|r| r.owner != 3));
                    }
                }
                node.expire(60_002);
                let stale = ReadRequest::Links(LinkRead {
                    reader: 1,
                    owner: 4,
                    request: 2,
                    via: 2,
                    after: 0,
                });
                assert!(node.receive_links(query(stale, NET, 8), 60_003).is_none());
            }
        }
        for (network, destination, source) in [
            (NET, None, 3),
            (NET, Some(2), 2),
            (Network { founder: 9, ..NET }, Some(2), 3),
        ] {
            let mut node = Forwarder::new(2, NET);
            let offer = Message {
                destination,
                ..envelope(
                    Content::LinkExchange {
                        network,
                        phase: LinkPhase::Offer,
                        token: 1,
                    },
                    source,
                )
            };
            assert!(node.receive_link(offer, 0).is_none());
            assert!(!node.links.contains(source));
        }
    }

    #[test]
    fn route_ranking() {
        let mut node = Forwarder::new(2, NET);
        for peer in 3..=6 {
            cost(&mut node, peer, 1);
        }
        for (next, hops, unit, expected) in [
            (3, 3, 41, [3, 0]),
            (4, 2, 40, [4, 3]),
            (5, 4, 10, [5, 4]),
            (6, 2, 99, [5, 4]),
            (4, 2, 5, [4, 5]),
            (4, 2, 80, [5, 4]),
        ] {
            node.learn(9, next, hops, 0, TimePerByte::measured(unit, 1));
            assert_eq!(node.next_hops(9, 1, 8), expected);
        }
        for (previous, remaining, expected) in [(5, 8, [4, 0]), (1, 2, [0, 0]), (1, 3, [4, 0])] {
            assert_eq!(node.next_hops(9, previous, remaining), expected);
        }
        link(&mut node, 9);
        cost(&mut node, 9, 90);
        node.learn(9, 3, 2, 2, TimePerByte::measured(2, 1));
        assert_eq!(node.next_hops(9, 1, 8), [3, 5]);
        for tail in [TimePerByte::UNKNOWN, TimePerByte::measured(100, 1)] {
            let mut node = Forwarder::new(2, NET);
            link(&mut node, 9);
            cost(&mut node, 9, 10);
            cost(&mut node, 3, 1);
            node.learn(9, 3, 2, 2, tail);
            assert_eq!(node.next_hops(9, 1, 8), [9, 3]);
        }
        assert_eq!(node.cost_to(2), TimePerByte::ZERO);
        assert_eq!(node.cost_to(99), TimePerByte::UNKNOWN);
    }

    #[test]
    fn call_fanout() {
        for directions in 0..=2 {
            for copies in [1, 2] {
                for remaining in [1, 8] {
                    let mut node = Forwarder::new(2, NET);
                    for next in 3..3 + directions {
                        node.learn(4, next, 2, 0, TimePerByte::UNKNOWN);
                    }
                    let request = envelope(
                        Content::Invoke {
                            network: NET,
                            call: CALL,
                            remaining,
                            copies,
                            arguments: &[0],
                        },
                        1,
                    );
                    let out = node
                        .receive_control(request, 1, |_, _| panic!("relay invoked native work"));
                    let count = if remaining == 1 {
                        0
                    } else {
                        directions.max(1).min(u64::from(copies)) as usize
                    };
                    assert_eq!(out.iter().flatten().count(), count);
                    for out in out.into_iter().flatten() {
                        let budget = if directions == 0 || count == 2 {
                            1
                        } else {
                            copies
                        };
                        assert!(
                            matches!(out.content, Content::Invoke { call, remaining: r, copies: c, arguments: &[0], .. } if call == CALL && r == remaining - 1 && c == budget)
                        );
                    }
                    assert_eq!(node.receive_control(request, 2, |_, _| panic!()), [None; 2]);
                }
            }
        }
    }

    #[test]
    fn call_correlation() {
        let request = envelope(
            Content::Invoke {
                network: NET,
                call: CALL,
                remaining: 8,
                copies: 2,
                arguments: &[0],
            },
            1,
        );
        let result = envelope(
            Content::InvocationResult {
                network: NET,
                call: CALL,
                status: InvokeStatus::Invoked,
                hops: 2,
            },
            3,
        );
        let mut node = Forwarder::new(2, NET);
        node.learn(4, 3, 2, 0, TimePerByte::UNKNOWN);
        node.learn(4, 5, 2, 0, TimePerByte::UNKNOWN);
        node.receive_control(request, 0, |_, _| panic!());
        for (source, remaining, args, joined) in [
            (7, 7, &[0][..], false),
            (3, 8, &[0][..], false),
            (6, 8, &[1][..], false),
            (6, 8, &[0][..], true),
            (7, 8, &[0][..], false),
        ] {
            let incoming = envelope(
                Content::Invoke {
                    network: NET,
                    call: CALL,
                    remaining,
                    copies: 1,
                    arguments: args,
                },
                source,
            );
            assert_eq!(
                node.receive_control(incoming, 1, |_, _| panic!()),
                [None; 2]
            );
            assert_eq!(node.calls[0].previous.contains(&source), joined);
        }
        for bad in [
            Message {
                source: 1,
                ..result
            },
            Message {
                source: 7,
                ..result
            },
            envelope(
                Content::InvocationResult {
                    network: NET,
                    call: Invocation {
                        transformer: 43,
                        ..CALL
                    },
                    status: InvokeStatus::Invoked,
                    hops: 2,
                },
                3,
            ),
            envelope(
                Content::InvocationResult {
                    network: NET,
                    call: CALL,
                    status: InvokeStatus::Invoked,
                    hops: MAX_HOPS,
                },
                3,
            ),
        ] {
            assert_eq!(node.receive_control(bad, 2, |_, _| panic!()), [None; 2]);
        }
        let returned = node.receive_control(result, 3, |_, _| panic!());
        assert_eq!(
            returned.map(|r| r.map(|m| m.destination)),
            [Some(Some(1)), Some(Some(6))]
        );
        assert_eq!(node.receive_control(result, 4, |_, _| panic!()), [None; 2]);
        assert!(node.receive_control(request, 5, |_, _| panic!())[0].is_some());
        let conflict = envelope(
            Content::InvocationResult {
                network: NET,
                call: CALL,
                status: InvokeStatus::Busy,
                hops: 2,
            },
            5,
        );
        assert_eq!(
            node.receive_control(conflict, 6, |_, _| panic!()),
            [None; 2]
        );
        node.expire(READ_MS);
        assert_eq!(
            node.next_hops(4, 1, 8),
            [3, 0],
            "only the unanswered direction expires"
        );
        for (local, network, caller, allowed) in [
            (4, NET, 1, true),
            (2, NET, 2, false),
            (4, Network { founder: 9, ..NET }, 1, false),
        ] {
            let mut owner = Forwarder::new(local, NET);
            let mut invoked = false;
            let input = envelope(
                Content::Invoke {
                    network,
                    call: Invocation { caller, ..CALL },
                    remaining: 1,
                    copies: 1,
                    arguments: &[0],
                },
                1,
            );
            owner.receive_control(input, 0, |_, _| {
                invoked = true;
                Some(InvokeStatus::Invoked)
            });
            assert_eq!(invoked, allowed);
        }
    }

    #[test]
    fn notice_scope() {
        for network in [NET, Network { founder: 9, ..NET }] {
            for owner in [2, 4] {
                for hops in [MAX_HOPS - 1, MAX_HOPS] {
                    let valid = network == NET && owner != 2;
                    let delta = StateChange {
                        owner,
                        attribute: 42,
                        sequence: 7,
                        value: None,
                    };
                    let arrival = Content::Arrive {
                        network,
                        owner: SurfaceOwner {
                            address: owner,
                            surface_bytes: 13,
                        },
                        sequence: 7,
                        hops,
                    };
                    for content in [
                        arrival,
                        Content::StateChanged {
                            network,
                            delta,
                            hops,
                        },
                    ] {
                        let mut node = Forwarder::new(2, NET);
                        let input = envelope(content, 3);
                        let mut sent = Vec::new();
                        for repeated in [false, true] {
                            let input = Message {
                                source: if repeated { 5 } else { 3 },
                                ..input
                            };
                            let accepted = match content {
                                Content::Arrive { .. } => node
                                    .receive_discovery(input, 13, 0, |m| sent.push(m))
                                    .is_some(),
                                _ => node.receive_change(input, 0, |m| sent.push(m)).is_some(),
                            };
                            assert_eq!(accepted, valid && !repeated);
                        }
                        assert_eq!(sent.len(), usize::from(valid && hops < MAX_HOPS));
                        assert!(node.routes.iter().all(|r| r.owner == 0));
                        if let Some(out) = sent.first() {
                            assert!(
                                matches!(out.content, Content::Arrive { hops: h, .. } | Content::StateChanged { hops: h, .. } if h == hops + 1)
                            );
                            assert_eq!(out.destination, None);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn discovery_scope() {
        for network in [NET, Network { founder: 9, ..NET }] {
            for seeker in [1, 2] {
                for remaining in [1, 2, MAX_HOPS] {
                    let mut node = Forwarder::new(2, NET);
                    let mut sent = Vec::new();
                    let find = envelope(
                        Content::FindOwners {
                            network,
                            seeker,
                            request: 7,
                            remaining,
                        },
                        1,
                    );
                    node.receive_discovery(find, 13, 0, |m| sent.push(m));
                    node.receive_discovery(find, 13, 1, |_| panic!("duplicate query forwarded"));
                    let valid = network == NET && seeker != 2;
                    assert_eq!(
                        sent.len(),
                        if valid {
                            1 + usize::from(remaining > 1)
                        } else {
                            0
                        }
                    );
                    if valid {
                        assert_eq!(sent[0].destination, Some(1));
                        assert!(matches!(
                            sent[0].content,
                            Content::OwnerFound {
                                owner: SurfaceOwner {
                                    address: 2,
                                    surface_bytes: 13
                                },
                                ..
                            }
                        ));
                        if sent.len() == 2 {
                            assert!(
                                matches!(sent[1].content, Content::FindOwners { remaining: r, .. } if r == remaining - 1)
                            );
                        }
                    }
                }
            }
        }
        for (seeker, request, source, hops, valid) in [
            (1, 7, 3, 2, true),
            (1, 8, 3, 2, false),
            (1, 7, 1, 2, false),
            (1, 7, 3, MAX_HOPS, false),
            (2, 7, 3, MAX_HOPS, true),
        ] {
            let mut node = Forwarder::new(2, NET);
            node.remember_discovery(seeker, 7, if seeker == 2 { 2 } else { 1 }, 0);
            for owner in [4, 5] {
                let found = envelope(
                    Content::OwnerFound {
                        network: NET,
                        seeker,
                        request,
                        owner: SurfaceOwner {
                            address: owner,
                            surface_bytes: 13,
                        },
                        hops,
                    },
                    source,
                );
                let mut sent = Vec::new();
                assert_eq!(
                    node.receive_discovery(found, 13, 1, |m| sent.push(m))
                        .is_some(),
                    valid
                );
                assert_eq!(sent.len(), usize::from(valid && seeker != 2));
                assert!(node.receive_discovery(found, 13, 2, |_| panic!()).is_none());
            }
            assert_eq!(
                node.routes.iter().filter(|r| r.owner != 0).count(),
                if valid { 2 } else { 0 }
            );
        }
    }

    #[test]
    fn storage_bounds() {
        let mut node = Forwarder::new(2, NET);
        for i in 1..=READ_PATHS
            .max(CALL_PATHS)
            .max(OWNER_NOTICES)
            .max(CHANGE_NOTICES)
            .max(DISCOVERY_PATHS)
            .max(ROUTES)
            + 1
        {
            let owner = i as u64 + 10;
            let now = i as u32;
            node.remember_read(reads(1, owner, i as u16)[i % 4], 1, 0, false, now);
            node.remember_call(
                Invocation {
                    request: i as u32,
                    ..CALL
                },
                &[0],
                1,
                [0; 2],
                8,
                now,
            );
            node.remember_discovery(1, i as u16, 1, now);
            node.notice(owner, 1, 1, now);
            node.change_seen(owner, 1, now);
            node.learn(owner, 3, 2, now, TimePerByte::UNKNOWN);
            for (count, capacity) in [
                (
                    node.reads.iter().filter(|r| r.read.reader() != 0).count(),
                    READ_PATHS,
                ),
                (
                    node.calls.iter().filter(|r| r.call.caller != 0).count(),
                    CALL_PATHS,
                ),
                (
                    node.discovery.iter().filter(|r| r.seeker != 0).count(),
                    DISCOVERY_PATHS,
                ),
                (
                    node.notices.iter().filter(|r| r.owner != 0).count(),
                    OWNER_NOTICES,
                ),
                (
                    node.changes.iter().filter(|r| r.owner != 0).count(),
                    CHANGE_NOTICES,
                ),
                (node.routes.iter().filter(|r| r.owner != 0).count(), ROUTES),
            ] {
                assert_eq!(count, i.min(capacity));
            }
            assert_eq!(
                node.reads.iter().any(|r| r.read.request() == 1),
                i <= READ_PATHS
            );
            assert_eq!(
                node.calls.iter().any(|r| r.call.request == 1),
                i <= CALL_PATHS
            );
            assert_eq!(
                node.discovery.iter().any(|r| r.request == 1),
                i <= DISCOVERY_PATHS
            );
            assert_eq!(
                node.notices.iter().any(|r| r.owner == 11),
                i <= OWNER_NOTICES
            );
            assert_eq!(
                node.changes.iter().any(|r| r.owner == 11),
                i <= CHANGE_NOTICES
            );
            assert_eq!(node.routes.iter().any(|r| r.owner == 11), i <= ROUTES);
        }
    }

    #[test]
    fn expiry_bounds() {
        for start in [0, u32::MAX - 5] {
            for elapsed in [READ_MS - 1, READ_MS, ROUTE_MS - 1, ROUTE_MS] {
                let mut node = Forwarder::new(2, NET);
                node.remember_read(reads(1, 4, 7)[0], 1, 0, false, start);
                node.remember_call(CALL, &[0], 1, [0; 2], 8, start);
                node.remember_discovery(1, 7, 1, start);
                node.notice(4, 1, 7, start);
                node.change_seen(4, 7, start);
                node.learn(4, 3, 2, start, TimePerByte::UNKNOWN);
                node.expire(start.wrapping_add(elapsed));
                for live in [
                    node.reads.iter().any(|r| r.read.reader() != 0),
                    node.calls.iter().any(|r| r.call.caller != 0),
                    node.discovery.iter().any(|r| r.seeker != 0),
                    node.notices.iter().any(|r| r.owner != 0),
                    node.changes.iter().any(|r| r.owner != 0),
                ] {
                    assert_eq!(live, elapsed < READ_MS);
                }
                assert_eq!(node.routes.iter().any(|r| r.owner != 0), elapsed < ROUTE_MS);
            }
        }
        // An older unanswered path cannot erase a newer confirmation.
        for confirmed in [0, 1, 2] {
            let mut node = Forwarder::new(2, NET);
            node.learn(4, 3, 2, confirmed, TimePerByte::UNKNOWN);
            node.remember_read(reads(1, 4, 7)[0], 1, 3, false, 1);
            node.expire(READ_MS + 1);
            assert_eq!(node.next_hop(4, 1, 8), (confirmed > 1).then_some(3));
        }
    }
}
