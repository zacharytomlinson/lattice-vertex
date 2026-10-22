//! Bounded host requests and registered-change delivery from the resident peer.
//! USB belongs to the board firmware; this module owns admission and buffering.
//! No allocation, configuration override, retry, or foreign-surface registry.

use super::{
    exposure::Description,
    forwarding::{Forwarder, ReadRequest},
    frame::{
        Content, Invocation, LinkRead, Message, ProgramRead, StateRead, SurfaceRead, UpdateRead,
    },
    invocation::Invocations,
};
use crate::kernel::appliance::Native;
use crate::kernel::radio::cost::TimePerByte;
use crate::{port, sync::TaskContext, types::UBaseType_t};
pub use lattice_peer_wire::host::{Arguments, Bytes, Decoder, Failure, Packet, MAX_PACKET};
pub use lattice_peer_wire::StateChange;
use lattice_peer_wire::SURFACE_BYTES;
const CHANGE_CAPACITY: usize = 8;
const READ_TIMEOUT_MS: u32 = 8_000;

struct Mailbox {
    generation: u32,
    busy: bool,
    request: Option<Packet>,
    response: Option<Packet>,
    attached: bool,
    changes: [Option<StateChange>; CHANGE_CAPACITY],
    first: usize,
    count: usize,
    dropped: u32,
}
impl Mailbox {
    const fn new() -> Self {
        Self {
            generation: 0,
            busy: false,
            request: None,
            response: None,
            attached: false,
            changes: [None; CHANGE_CAPACITY],
            first: 0,
            count: 0,
            dropped: 0,
        }
    }
}
static mut MAILBOX: Mailbox = Mailbox::new();
fn access<R>(f: impl FnOnce(&mut Mailbox) -> R) -> R {
    struct Restore(UBaseType_t);
    impl Drop for Restore {
        fn drop(&mut self) {
            port::portCLEAR_INTERRUPT_MASK_FROM_ISR(self.0);
        }
    }
    let _restore = Restore(port::portSET_INTERRUPT_MASK_FROM_ISR());
    // Both MCU ports are single-core. Keep the interrupt-masked copy short.
    f(unsafe { &mut *core::ptr::addr_of_mut!(MAILBOX) })
}

/// Exactly one request may be outstanding, including a response waiting for USB.
/// Call from the one owning host-transport task, never from an ISR.
pub fn submit(_context: &TaskContext, request: Packet) -> Result<(), Failure> {
    if !request.is_request() {
        return Err(Failure::InvalidRequest);
    }
    access(|m| {
        if m.busy {
            return Err(Failure::Busy);
        }
        m.busy = true;
        m.request = Some(request);
        Ok(())
    })?;
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    crate::kernel::radio::task::notify_work();
    Ok(())
}
/// Start delivery of subsequent changes. No snapshot or pre-attachment replay.
pub fn connect(_context: &TaskContext) {
    access(|m| m.attached = true);
}
/// Drain a reply first, then any loss notice, then registered changes in order.
pub fn receive(_context: &TaskContext) -> Option<Packet> {
    access(|m| {
        if let Some(response) = m.response.take() {
            m.busy = false;
            return Some(response);
        }
        if m.dropped != 0 {
            return Some(Packet::ChangesLost {
                dropped: core::mem::take(&mut m.dropped),
            });
        }
        if m.count == 0 {
            return None;
        }
        let change = m.changes[m.first].take().unwrap();
        m.first = (m.first + 1) % CHANGE_CAPACITY;
        m.count -= 1;
        Some(Packet::Changed { change })
    })
}
/// Called only for a locally registered or accepted, deduplicated remote delta.
pub(super) fn publish(change: StateChange) {
    access(|m| {
        if !m.attached {
            return;
        }
        if m.count == CHANGE_CAPACITY {
            m.changes[m.first] = None;
            m.first = (m.first + 1) % CHANGE_CAPACITY;
            m.count -= 1;
            m.dropped = m.dropped.saturating_add(1);
        }
        m.changes[(m.first + m.count) % CHANGE_CAPACITY] = Some(change);
        m.count += 1;
    });
}
/// Abandon the USB session. Old radio completions cannot satisfy a new session.
pub fn disconnect(_context: &TaskContext) {
    access(|m| {
        let generation = m.generation.wrapping_add(1);
        *m = Mailbox::new();
        m.generation = generation;
    });
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    crate::kernel::radio::task::notify_work();
}
pub(crate) fn pending() -> bool {
    access(|m| m.request.is_some())
}
pub(crate) fn staging_pending() -> bool {
    access(|m| {
        matches!(
            m.request,
            Some(Packet::Update { .. } | Packet::Program { .. })
        )
    })
}
fn deliver(generation: u32, packet: Packet) {
    access(|m| {
        if m.generation == generation && m.busy && m.request.is_none() && m.response.is_none() {
            m.response = Some(packet);
        }
    });
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Read(ReadRequest),
    Control,
    Call(Invocation),
    LocalCall(Invocation),
}
impl Operation {
    fn cancel(self, forwarder: &mut Forwarder) {
        match self {
            Self::Control | Self::LocalCall(_) => {}
            Self::Read(read) => forwarder.cancel_read(read),
            Self::Call(call) => forwarder.cancel_call(call),
        }
    }
}
struct Pending {
    generation: u32,
    host_token: u32,
    operation: Operation,
    started: u32,
}
pub(super) struct Host {
    next_call: u32,
    calls_seeded: bool,
    call_ids: [(u32, u32); 8],
    next_call_slot: usize,
    pending: Option<Pending>,
    payload: [u8; lattice_peer_wire::update::REQUEST_BYTES],
}
impl Host {
    pub(super) const fn new() -> Self {
        Self {
            next_call: 0,
            calls_seeded: false,
            call_ids: [(0, 0); 8],
            next_call_slot: 0,
            pending: None,
            payload: [0; lattice_peer_wire::update::REQUEST_BYTES],
        }
    }
    // Host tokens and radio call IDs are separate. The resident Host survives
    // USB reconnects, retaining the eight most recent mappings. Evicted tokens
    // get fresh IDs and must not be used to retry an uncertain mutation.
    fn call_request(&mut self, token: u32) -> Option<u32> {
        if let Some(&(_, id)) = self.call_ids.iter().find(|&&(t, id)| id != 0 && t == token) {
            return Some(id);
        }
        if !self.calls_seeded {
            self.next_call = (token & 0x7fff_ffff).max(1);
            self.calls_seeded = true;
        }
        if self.next_call == 0 {
            return None;
        }
        let request = self.next_call;
        self.next_call = request.checked_add(1).unwrap_or(0);
        self.call_ids[self.next_call_slot] = (token, request);
        self.next_call_slot = (self.next_call_slot + 1) % self.call_ids.len();
        Some(request)
    }

    pub(super) fn poll<'a>(
        &'a mut self,
        forwarder: &mut Forwarder,
        description: &Description,
        native: &'static dyn Native,
        invocations: &mut Invocations,
        context: &TaskContext,
        now: u32,
    ) -> Option<Message<'a>> {
        if let Some(p) = &self.pending {
            if let Operation::LocalCall(call) = p.operation {
                if let Some(status) = invocations.result(call) {
                    let p = self.pending.take().unwrap();
                    deliver(
                        p.generation,
                        Packet::Invoked {
                            token: p.host_token,
                            owner: call.owner,
                            transformer: call.transformer,
                            status,
                        },
                    );
                }
            }
        }
        if let Some(p) = &self.pending {
            if p.operation == Operation::Control {
                if let Some((progress, release)) =
                    crate::kernel::peer::update_control_complete(context)
                {
                    let p = self.pending.take().unwrap();
                    deliver(
                        p.generation,
                        Packet::UpdateControlResult {
                            token: p.host_token,
                            progress,
                            release,
                        },
                    );
                }
            }
        }
        if let Some(pending) = &self.pending {
            let connected = access(|m| m.generation == pending.generation);
            if !connected || now.wrapping_sub(pending.started) >= READ_TIMEOUT_MS {
                let pending = self.pending.take().unwrap();
                pending.operation.cancel(forwarder);
                if connected {
                    deliver(
                        pending.generation,
                        Packet::Error {
                            token: pending.host_token,
                            failure: Failure::Timeout,
                        },
                    );
                }
            }
        }
        let (generation, request) = access(|m| m.request.take().map(|r| (m.generation, r)))?;
        let (local, network) = forwarder.identity();
        let token = request.token();
        if let Packet::Status { .. } = request {
            deliver(
                generation,
                Packet::Ready {
                    token,
                    address: local,
                    founder: network.founder,
                    channel: network.channel,
                },
            );
            return None;
        }
        if let Packet::UpdateControl { action, .. } = request {
            use lattice_peer_wire::update::Progress;
            match crate::kernel::peer::update_control(context, action) {
                Ok(Some((progress, release))) => deliver(
                    generation,
                    Packet::UpdateControlResult {
                        token,
                        progress,
                        release,
                    },
                ),
                Ok(None) => {
                    self.pending = Some(Pending {
                        generation,
                        host_token: token,
                        operation: Operation::Control,
                        started: now,
                    })
                }
                Err(status) => {
                    let mut progress = Progress::unsupported([0; 16]);
                    progress.status = status;
                    deliver(
                        generation,
                        Packet::UpdateControlResult {
                            token,
                            progress,
                            release: 0,
                        },
                    );
                }
            }
            return None;
        }
        let owner = match request {
            Packet::ReadLinks { owner, .. }
            | Packet::ReadSurface { owner, .. }
            | Packet::ReadState { owner, .. }
            | Packet::Invoke { owner, .. }
            | Packet::Program { owner, .. }
            | Packet::Update { owner, .. } => owner,
            _ => unreachable!("submit rejects replies"),
        };
        let valid = |a| a != 0 && a != u64::MAX;
        let invalid = !valid(owner)
            || match request {
                Packet::ReadLinks { after, via, .. } => {
                    after == u64::MAX || (via != 0 && (!valid(via) || via == local || via == owner))
                }
                Packet::ReadState { attribute, .. } => !valid(attribute),
                Packet::Invoke { transformer, .. } => !valid(transformer),
                Packet::Update { data, .. } => {
                    lattice_peer_wire::update::Request::decode(data.as_slice()).is_none()
                }
                Packet::Program { data, .. } => {
                    lattice_peer_wire::program::transfer::Request::decode(data.as_slice()).is_none()
                }
                _ => false,
            };
        if invalid {
            deliver(
                generation,
                Packet::Error {
                    token,
                    failure: Failure::InvalidRequest,
                },
            );
            return None;
        }
        #[cfg(feature = "firmware-update")]
        if !matches!(request, Packet::Update { .. } | Packet::Program { .. }) {
            crate::kernel::update::activation::activity();
        }
        if let Packet::Invoke {
            transformer,
            arguments,
            ..
        } = request
        {
            // Preserve the retained mapping without recycling exhausted IDs.
            let Some(request) = self.call_request(token) else {
                deliver(
                    generation,
                    Packet::Error {
                        token,
                        failure: Failure::Busy,
                    },
                );
                return None;
            };
            let call = Invocation {
                caller: local,
                owner,
                request,
                transformer,
            };
            if owner == local {
                if let Some(status) =
                    invocations.submit(native, context, call, arguments.as_slice())
                {
                    deliver(
                        generation,
                        Packet::Invoked {
                            token,
                            owner,
                            transformer,
                            status,
                        },
                    );
                } else {
                    self.pending = Some(Pending {
                        generation,
                        host_token: token,
                        operation: Operation::LocalCall(call),
                        started: now,
                    });
                }
            } else {
                let args = arguments.as_slice();
                self.payload[..args.len()].copy_from_slice(args);
                if let Some(outgoing) =
                    forwarder.originate_call(call, &self.payload[..args.len()], now)
                {
                    self.pending = Some(Pending {
                        generation,
                        host_token: token,
                        operation: Operation::Call(call),
                        started: now,
                    });
                    return Some(outgoing);
                }
                deliver(
                    generation,
                    Packet::Error {
                        token,
                        failure: Failure::InvalidRequest,
                    },
                );
            }
            return None;
        }
        if let Packet::Program { data, .. } = request {
            let operation =
                lattice_peer_wire::program::transfer::Request::decode(data.as_slice()).unwrap();
            if owner == local {
                let mut response = [0; lattice_peer_wire::program::transfer::REPLY_BYTES];
                let n = crate::kernel::program::runtime::receive(local, operation, &mut response);
                deliver(
                    generation,
                    Packet::ProgramResult {
                        token,
                        owner,
                        data: Bytes::new(&response[..n]).unwrap(),
                    },
                );
            } else {
                let radio_token = forwarder.request_token(token as u16);
                let read = ProgramRead {
                    reader: local,
                    owner,
                    request: radio_token,
                };
                self.payload[..data.as_slice().len()].copy_from_slice(data.as_slice());
                if let Some(outgoing) =
                    forwarder.originate_program(read, &self.payload[..data.as_slice().len()], now)
                {
                    self.pending = Some(Pending {
                        generation,
                        host_token: token,
                        operation: Operation::Read(ReadRequest::Program(read)),
                        started: now,
                    });
                    return Some(outgoing);
                }
                deliver(
                    generation,
                    Packet::Error {
                        token,
                        failure: Failure::InvalidRequest,
                    },
                );
            }
            return None;
        }
        if let Packet::Update { data, .. } = request {
            let operation = lattice_peer_wire::update::Request::decode(data.as_slice()).unwrap();
            if owner == local {
                let progress = crate::kernel::peer::handle_update(
                    operation,
                    forwarder.ordinary_work_pending(),
                );
                deliver(
                    generation,
                    Packet::Updated {
                        token,
                        owner,
                        progress,
                        cost: TimePerByte::ZERO,
                    },
                );
            } else {
                let radio_token = forwarder.request_token(token as u16);
                let read = UpdateRead {
                    reader: local,
                    owner,
                    request: radio_token,
                    package: operation.package(),
                };
                self.payload[..data.as_slice().len()].copy_from_slice(data.as_slice());
                if let Some(outgoing) =
                    forwarder.originate_update(read, &self.payload[..data.as_slice().len()], now)
                {
                    self.pending = Some(Pending {
                        generation,
                        host_token: token,
                        operation: Operation::Read(ReadRequest::Update(read)),
                        started: now,
                    });
                    return Some(outgoing);
                }
                deliver(
                    generation,
                    Packet::Error {
                        token,
                        failure: Failure::InvalidRequest,
                    },
                );
            }
            return None;
        }
        if owner == local {
            let response = match request {
                Packet::ReadLinks { after, via, .. } => {
                    let (revision, link) = forwarder.local_links(after);
                    Packet::Link {
                        token,
                        owner,
                        after,
                        via,
                        revision,
                        peer: link.map_or(0, |l| l.peer),
                        round_trip_ms: link.map_or(0, |l| l.round_trip_ms),
                        cost: link.map_or(TimePerByte::UNKNOWN, |l| l.cost),
                    }
                }
                Packet::ReadSurface { offset, .. } if offset <= description.len() => {
                    let mut bytes = [0; SURFACE_BYTES];
                    let n = description.encode_part(offset, &mut bytes);
                    Packet::Surface {
                        token,
                        owner,
                        offset,
                        total: description.len(),
                        data: Bytes::new(&bytes[..n]).unwrap(),
                    }
                }
                Packet::ReadState { attribute, .. } => Packet::State {
                    token,
                    owner,
                    attribute,
                    result: native.read(context, attribute),
                },
                _ => Packet::Error {
                    token,
                    failure: Failure::InvalidRequest,
                },
            };
            deliver(generation, response);
            return None;
        }
        // A reboot must not deliberately reuse read 1 while another resident
        // still holds that caller/token. Seed once from the host's random token,
        // then keep one monotonic namespace across read kinds and USB sessions.
        let radio_token = forwarder.request_token(token as u16);
        let read = match request {
            Packet::ReadLinks { after, via, .. } => ReadRequest::Links(LinkRead {
                reader: local,
                owner,
                request: radio_token,
                after,
                via,
            }),
            Packet::ReadSurface { offset, .. } => ReadRequest::Surface(SurfaceRead {
                reader: local,
                owner,
                request: radio_token,
                offset,
            }),
            Packet::ReadState { attribute, .. } => ReadRequest::State(StateRead {
                reader: local,
                owner,
                request: radio_token,
                attribute,
            }),
            _ => unreachable!(),
        };
        if let Some(outgoing) = forwarder.originate_read(read, now) {
            self.pending = Some(Pending {
                generation,
                host_token: token,
                operation: Operation::Read(read),
                started: now,
            });
            return Some(outgoing);
        }
        deliver(
            generation,
            Packet::Error {
                token,
                failure: Failure::InvalidRequest,
            },
        );
        None
    }
    pub(super) fn wait_ms(&self, now: u32) -> Option<u32> {
        self.pending
            .as_ref()
            .map(|r| READ_TIMEOUT_MS.saturating_sub(now.wrapping_sub(r.started)))
    }
    pub(super) fn transmit_failed(&mut self, forwarder: &mut Forwarder) {
        if let Some(pending) = self.pending.take() {
            pending.operation.cancel(forwarder);
            deliver(
                pending.generation,
                Packet::Error {
                    token: pending.host_token,
                    failure: Failure::Transmit,
                },
            );
        }
    }
    pub(super) fn complete(&mut self, forwarder: &mut Forwarder, incoming: Message<'_>, now: u32) {
        let Some(pending) = &self.pending else {
            return;
        };
        let token = pending.host_token;
        let (operation, response) = if let Content::InvocationResult { .. } = incoming.content {
            let Some((call, status)) = forwarder.complete_call(incoming, now) else {
                return;
            };
            (
                Operation::Call(call),
                Packet::Invoked {
                    token,
                    owner: call.owner,
                    transformer: call.transformer,
                    status,
                },
            )
        } else {
            let Some(message) = forwarder.complete_read(incoming, now) else {
                return;
            };
            match message.content {
                Content::ProgramResult { read, bytes, .. } => (
                    Operation::Read(ReadRequest::Program(read)),
                    Packet::ProgramResult {
                        token,
                        owner: read.owner,
                        data: Bytes::new(bytes).unwrap(),
                    },
                ),
                Content::Updated { read, progress, .. } => (
                    Operation::Read(ReadRequest::Update(read)),
                    Packet::Updated {
                        token,
                        owner: read.owner,
                        progress,
                        cost: forwarder.cost_to(read.owner),
                    },
                ),
                Content::LinkEntry {
                    read,
                    revision,
                    link,
                    ..
                } => (
                    Operation::Read(ReadRequest::Links(read)),
                    Packet::Link {
                        token,
                        owner: read.owner,
                        after: read.after,
                        via: read.via,
                        revision,
                        peer: link.map_or(0, |l| l.peer),
                        round_trip_ms: link.map_or(0, |l| l.round_trip_ms),
                        cost: link.map_or(TimePerByte::UNKNOWN, |l| l.cost),
                    },
                ),
                Content::SurfacePart {
                    read, total, bytes, ..
                } => (
                    Operation::Read(ReadRequest::Surface(read)),
                    Packet::Surface {
                        token,
                        owner: read.owner,
                        offset: read.offset,
                        total,
                        data: Bytes::new(bytes).unwrap(),
                    },
                ),
                Content::StateValue { read, result, .. } => (
                    Operation::Read(ReadRequest::State(read)),
                    Packet::State {
                        token,
                        owner: read.owner,
                        attribute: read.attribute,
                        result,
                    },
                ),
                _ => return,
            }
        };
        if pending.operation != operation {
            return;
        }
        let pending = self.pending.take().unwrap();
        deliver(pending.generation, response);
    }
}

#[cfg(test)]
mod tests;
