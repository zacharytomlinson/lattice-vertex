//! Pinned directions for native control. Setup uses ordinary routed exchanges;
//! samples and feedback never discover or repair a route.
use super::*;
use lattice_peer_wire::{
    control::{Request as ControlRequest, State, Stream, Ticket},
    program::transfer::{Reply, Request, Status},
};
pub(super) const STREAMS: usize = 4;
const IDLE_MS: u32 = 30_000;
#[derive(Clone, Copy)]
pub(super) struct StreamPath {
    id: u64,
    read: ProgramRead,
    previous: u64,
    onward: u64,
    ticket: Option<Ticket>,
    touched: u32,
    closing: bool,
}
impl StreamPath {
    pub const EMPTY: Self = Self {
        id: 0,
        read: ProgramRead {
            reader: 0,
            owner: 0,
            request: 0,
        },
        previous: 0,
        onward: 0,
        ticket: None,
        touched: 0,
        closing: false,
    };
}
const _: () = assert!(core::mem::size_of::<StreamPath>() * STREAMS <= 448);
impl Forwarder {
    pub(super) fn expire_streams(&mut self, now: u32) {
        for p in &mut self.streams {
            if p.id != 0 && now.wrapping_sub(p.touched) >= IDLE_MS {
                *p = StreamPath::EMPTY;
            }
        }
    }
    /// Reserve before forwarding Open, so a successful reply implies that every
    /// participating relay had capacity. Never evict another active stream.
    pub(super) fn stream_setup(
        &mut self,
        read: ProgramRead,
        bytes: &[u8],
        previous: u64,
        proposed: u64,
        now: u32,
    ) -> Result<u64, Status> {
        let Some(Request::Control(request)) = Request::decode(bytes) else {
            return Ok(proposed);
        };
        let index = match request {
            ControlRequest::Open { nonce, .. } => {
                if nonce == 0 {
                    return Err(Status::Invalid);
                }
                if let Some(i) = self.streams.iter().position(|p| p.id == nonce) {
                    let p = self.streams[i];
                    if p.read.reader != read.reader
                        || p.read.owner != read.owner
                        || p.previous != previous
                        || p.closing
                    {
                        return Err(Status::Conflict);
                    }
                    i
                } else {
                    let i = self
                        .streams
                        .iter()
                        .position(|p| p.id == 0)
                        .ok_or(Status::Busy)?;
                    self.streams[i] = StreamPath {
                        id: nonce,
                        read,
                        previous,
                        onward: if read.owner == self.local {
                            self.local
                        } else {
                            proposed
                        },
                        touched: now,
                        ..StreamPath::EMPTY
                    };
                    i
                }
            }
            ControlRequest::Finish { ticket, .. } | ControlRequest::Close { ticket, .. } => self
                .streams
                .iter()
                .position(|p| {
                    p.id != 0
                        && p.read.reader == read.reader
                        && p.read.owner == read.owner
                        && p.previous == previous
                        && p.ticket == Some(ticket)
                })
                .ok_or(Status::Stale)?,
            // Old full-ticket sample/feedback records are not routed stream data.
            _ => return Err(Status::Unsupported),
        };
        let p = &mut self.streams[index];
        if matches!(request, ControlRequest::Close { nonce, .. } if nonce != p.id) {
            return Err(Status::Conflict);
        }
        p.read = read;
        p.touched = now;
        p.closing = matches!(request, ControlRequest::Close { .. });
        Ok(p.onward)
    }
    pub(super) fn stream_response(
        &mut self,
        read: ProgramRead,
        source: u64,
        bytes: &[u8],
        now: u32,
    ) {
        let Some(p) = self
            .streams
            .iter_mut()
            .find(|p| p.id != 0 && p.read == read)
        else {
            return;
        };
        let Some(reply) = Reply::decode(bytes) else {
            return;
        };
        if p.closing {
            if matches!(reply.status, Status::Ok | Status::Stale) {
                *p = StreamPath::EMPTY;
            } else {
                p.closing = false;
            }
            return;
        }
        if let Some(state) =
            State::decode(reply).filter(|_| matches!(reply.status, Status::Ok | Status::Busy))
        {
            if p.ticket.is_some_and(|t| t != state.ticket) {
                return;
            }
            if p.onward != 0 && p.onward != source {
                return;
            }
            p.ticket = Some(state.ticket);
            p.onward = source;
            p.touched = now;
        } else if p.ticket.is_none() {
            *p = StreamPath::EMPTY;
        }
    }
    pub(in crate::kernel::peer) fn stream_binding(
        &self,
        caller: u64,
        owner: u64,
        ticket: Ticket,
    ) -> Option<u64> {
        self.streams
            .iter()
            .find(|p| {
                p.id != 0
                    && p.read.reader == caller
                    && p.read.owner == owner
                    && p.ticket == Some(ticket)
            })
            .map(|p| p.id)
    }
    pub(in crate::kernel::peer) fn originate_stream(
        &mut self,
        remote: u64,
        packet: Stream,
        now: u32,
    ) -> Option<Message<'static>> {
        self.expire_streams(now);
        let p = self
            .streams
            .iter_mut()
            .find(|p| p.id == packet.id() && p.ticket.is_some())?;
        let next = match packet {
            Stream::Target { .. } | Stream::Receipt { .. }
                if p.read.reader == self.local && p.read.owner == remote =>
            {
                p.onward
            }
            Stream::Progress { .. } if p.read.owner == self.local && p.read.reader == remote => {
                p.previous
            }
            _ => return None,
        };
        if next == 0 || next == self.local || p.closing {
            return None;
        }
        p.touched = now;
        Some(Message {
            source: self.local,
            destination: Some(next),
            path_cost: TimePerByte::UNKNOWN,
            content: Content::Stream(packet),
        })
    }
    pub(in crate::kernel::peer) fn receive_stream(
        &mut self,
        incoming: Message<'_>,
        now: u32,
        mut handle: impl FnMut(u64, Stream) -> Option<Stream>,
    ) -> Option<Message<'static>> {
        let Content::Stream(packet) = incoming.content else {
            return None;
        };
        if incoming.destination != Some(self.local) {
            return None;
        }
        self.expire_streams(now);
        let response = |destination, packet| Message {
            source: self.local,
            destination: Some(destination),
            path_cost: TimePerByte::UNKNOWN,
            content: Content::Stream(packet),
        };
        let Some(p) = self.streams.iter_mut().find(|p| p.id == packet.id()) else {
            return (!matches!(packet, Stream::Fault { .. })).then(|| {
                response(
                    incoming.source,
                    Stream::Fault {
                        stream: packet.id(),
                        status: Status::Stale,
                    },
                )
            });
        };
        // Data can overtake a lost Open reply. A reserved path is not yet a
        // usable binding, but must remain available for the setup retry.
        if p.ticket.is_none() {
            return None;
        }
        let forward = incoming.source == p.previous && p.previous != self.local;
        let reverse = incoming.source == p.onward && p.onward != self.local;
        let valid = match packet {
            Stream::Target { .. } | Stream::Receipt { .. } => forward,
            Stream::Progress { .. } => reverse,
            Stream::Fault { .. } => forward || reverse,
        };
        if !valid {
            return None;
        }
        #[cfg(feature = "firmware-update")]
        crate::kernel::update::activation::activity();
        let next = if forward { p.onward } else { p.previous };
        let remote = if forward { p.read.reader } else { p.read.owner };
        p.touched = now;
        if matches!(packet, Stream::Fault { .. }) {
            *p = StreamPath::EMPTY;
        }
        if next == self.local {
            let reply = handle(remote, packet)?;
            if matches!(reply, Stream::Fault { .. }) {
                *p = StreamPath::EMPTY;
            }
            Some(response(incoming.source, reply))
        } else if next != 0 {
            Some(response(next, packet))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_peer_wire::{
        control::{Argument, Progress},
        InvokeStatus, StateReadResult, Type, Value,
    };
    const NET: Network = Network {
        founder: 1,
        channel: 15,
    };
    const TICKET: Ticket = Ticket {
        epoch: 7,
        slot: 0,
        generation: 1,
    };
    fn state() -> State {
        State {
            ticket: TICKET,
            input: Type::Bool,
            revision: 1,
            progress: Progress {
                received: 0,
                completed: 0,
                result: InvokeStatus::Busy,
                finished: false,
                state: StateReadResult::Busy,
            },
        }
    }
    #[test]
    fn binding_bounds() {
        let mut bytes = [0; lattice_peer_wire::program::transfer::REQUEST_BYTES];
        let mut body = [0; 48];
        let mut reply = [0; lattice_peer_wire::program::transfer::REPLY_BYTES];
        let n = state().reply(Status::Ok, &mut body).encode(&mut reply);
        for count in [0, 1, STREAMS, STREAMS + 1] {
            let mut node = Forwarder::new(2, NET);
            for id in 1..=count {
                let len = Request::Control(ControlRequest::Open {
                    epoch: 7,
                    nonce: id as u64,
                    transformer: 8,
                    state: 9,
                })
                .encode(&mut bytes);
                let read = ProgramRead {
                    reader: 1,
                    owner: 3,
                    request: id as u16,
                };
                assert_eq!(
                    node.stream_setup(read, &bytes[..len], 1, 3, 0),
                    if id <= STREAMS {
                        Ok(3)
                    } else {
                        Err(Status::Busy)
                    }
                );
                node.stream_response(read, 3, &reply[..n], 0);
            }
            assert_eq!(
                node.streams.iter().filter(|p| p.id != 0).count(),
                count.min(STREAMS)
            );
        }
        for elapsed in [0, IDLE_MS - 1, IDLE_MS] {
            let start = u32::MAX - 10;
            let mut node = Forwarder::new(2, NET);
            let read = ProgramRead {
                reader: 1,
                owner: 3,
                request: 1,
            };
            let len = Request::Control(ControlRequest::Open {
                epoch: 7,
                nonce: 5,
                transformer: 8,
                state: 9,
            })
            .encode(&mut bytes);
            assert_eq!(node.stream_setup(read, &bytes[..len], 1, 3, start), Ok(3));
            assert_eq!(
                node.stream_setup(read, &bytes[..len], 4, 3, start),
                Err(Status::Conflict)
            );
            node.stream_response(read, 3, &reply[..n], start);
            // Normal route/history expiry never causes per-sample route lookup.
            node.expire(start.wrapping_add(elapsed));
            let packet = Stream::Target {
                stream: 5,
                sequence: 1,
                argument: Argument::from_value(Value::boolean(true)),
            };
            let m = Message {
                source: 1,
                destination: Some(2),
                path_cost: TimePerByte::UNKNOWN,
                content: Content::Stream(packet),
            };
            let got = node
                .receive_stream(m, start.wrapping_add(elapsed), |_, _| {
                    panic!("relay must not invoke native work")
                })
                .unwrap();
            assert_eq!(got.destination, Some(if elapsed < IDLE_MS { 3 } else { 1 }));
            assert_eq!(
                got.content,
                if elapsed < IDLE_MS {
                    Content::Stream(packet)
                } else {
                    Content::Stream(Stream::Fault {
                        stream: 5,
                        status: Status::Stale,
                    })
                }
            );
        }
    }
    #[test]
    fn bound_forwarding() {
        let mut relay = Forwarder::new(2, NET);
        let mut bytes = [0; lattice_peer_wire::program::transfer::REQUEST_BYTES];
        let mut body = [0; 48];
        let mut out = [0; lattice_peer_wire::program::transfer::REPLY_BYTES];
        let mut response = [0; lattice_peer_wire::program::transfer::REPLY_BYTES];
        let read = ProgramRead {
            reader: 1,
            owner: 3,
            request: 1,
        };
        let len = Request::Control(ControlRequest::Open {
            epoch: 7,
            nonce: 5,
            transformer: 8,
            state: 9,
        })
        .encode(&mut bytes);
        let setup = Message {
            source: 1,
            destination: Some(2),
            path_cost: TimePerByte::UNKNOWN,
            content: Content::Program {
                network: NET,
                read,
                remaining: 8,
                bytes: &bytes[..len],
            },
        };
        assert!(relay
            .receive_program(setup, 0, &mut out, |_, _, _| panic!("not owner"))
            .is_some());
        let early = Message {
            source: 3,
            destination: Some(2),
            path_cost: TimePerByte::UNKNOWN,
            content: Content::Stream(Stream::Progress {
                stream: 5,
                revision: 1,
                progress: state().progress,
            }),
        };
        assert!(relay
            .receive_stream(early, 0, |_, _| unreachable!())
            .is_none());
        let n = state().reply(Status::Ok, &mut body).encode(&mut response);
        let result = Message {
            source: 3,
            destination: Some(2),
            path_cost: TimePerByte::ZERO,
            content: Content::ProgramResult {
                network: NET,
                read,
                hops: 1,
                bytes: &response[..n],
            },
        };
        assert_eq!(
            relay
                .receive_program(result, 1, &mut out, |_, _, _| unreachable!())
                .unwrap()
                .destination,
            Some(1)
        );
        for (from, packet, to) in [
            (
                1,
                Stream::Target {
                    stream: 5,
                    sequence: 1,
                    argument: Argument::from_value(Value::boolean(true)),
                },
                3,
            ),
            (
                3,
                Stream::Progress {
                    stream: 5,
                    revision: 1,
                    progress: state().progress,
                },
                1,
            ),
            (
                1,
                Stream::Receipt {
                    stream: 5,
                    revision: 1,
                },
                3,
            ),
        ] {
            let m = Message {
                source: from,
                destination: Some(2),
                path_cost: TimePerByte::UNKNOWN,
                content: Content::Stream(packet),
            };
            assert_eq!(
                relay
                    .receive_stream(m, 2, |_, _| panic!("not owner"))
                    .unwrap()
                    .destination,
                Some(to)
            );
            assert!(relay
                .receive_stream(Message { source: 4, ..m }, 2, |_, _| unreachable!())
                .is_none());
        }
        // Setup retries stay pinned even if normal routing suggests a different peer.
        let retry = ProgramRead { request: 2, ..read };
        assert_eq!(relay.stream_setup(retry, &bytes[..len], 1, 4, 3), Ok(3));
        let wrong = Request::Control(ControlRequest::Close {
            ticket: TICKET,
            nonce: 6,
        })
        .encode(&mut bytes);
        assert_eq!(
            relay.stream_setup(ProgramRead { request: 3, ..read }, &bytes[..wrong], 1, 4, 4),
            Err(Status::Conflict)
        );
        let len = Request::Control(ControlRequest::Close {
            ticket: TICKET,
            nonce: 5,
        })
        .encode(&mut bytes);
        assert_eq!(
            relay.stream_setup(ProgramRead { request: 3, ..read }, &bytes[..len], 1, 4, 4),
            Ok(3)
        );
        let n = Reply::status(Status::Ok).encode(&mut response);
        relay.stream_response(ProgramRead { request: 3, ..read }, 3, &response[..n], 5);
        assert!(relay.stream_binding(1, 3, TICKET).is_none());
        let fault = Message {
            source: 3,
            destination: Some(2),
            path_cost: TimePerByte::UNKNOWN,
            content: Content::Stream(Stream::Fault {
                stream: 5,
                status: Status::Stale,
            }),
        };
        assert!(relay
            .receive_stream(fault, 6, |_, _| unreachable!())
            .is_none());
    }
}
