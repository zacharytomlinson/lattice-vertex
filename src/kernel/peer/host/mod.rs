//! One bounded local host exchange with the resident peer. USB belongs to the
//! board firmware; this module owns request admission and the local read origin.
//! No allocation, configuration override, retry, or foreign-surface registry.

use super::{
    forwarding::Forwarder,
    frame::{LinkRead, Message},
    Network,
};
use crate::{port, sync::TaskContext, types::UBaseType_t};
pub use lattice_host_link::{Decoder, Failure, Packet, MAX_PACKET};
const READ_TIMEOUT_MS: u32 = 8_000;

struct Mailbox {
    generation: u32,
    busy: bool,
    request: Option<Packet>,
    response: Option<Packet>,
}
impl Mailbox {
    const fn new() -> Self {
        Self {
            generation: 0,
            busy: false,
            request: None,
            response: None,
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
pub fn receive(_context: &TaskContext) -> Option<Packet> {
    access(|m| {
        let result = m.response.take()?;
        m.busy = false;
        Some(result)
    })
}
/// Abandon the USB session. Old radio completions cannot satisfy a new session.
pub fn disconnect(_context: &TaskContext) {
    access(|m| {
        m.generation = m.generation.wrapping_add(1);
        m.busy = false;
        m.request = None;
        m.response = None;
    });
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    crate::kernel::radio::task::notify_work();
}
pub(crate) fn pending() -> bool {
    access(|m| m.request.is_some())
}
fn deliver(generation: u32, packet: Packet) {
    access(|m| {
        if m.generation == generation && m.busy && m.request.is_none() && m.response.is_none() {
            m.response = Some(packet);
        }
    });
}
struct PendingRead {
    generation: u32,
    host_token: u32,
    read: LinkRead,
    started: u32,
}
pub(super) struct Host {
    next_token: u16,
    read: Option<PendingRead>,
}
impl Host {
    pub(super) const fn new() -> Self {
        Self {
            next_token: 0,
            read: None,
        }
    }
    pub(super) fn poll(
        &mut self,
        forwarder: &mut Forwarder,
        local: u64,
        network: Network,
        now: u32,
    ) -> Option<Message<'static>> {
        if let Some(read) = &self.read {
            let connected = access(|m| m.generation == read.generation);
            if !connected || now.wrapping_sub(read.started) >= READ_TIMEOUT_MS {
                let read = self.read.take().unwrap();
                forwarder.cancel_links(read.read);
                if connected {
                    deliver(
                        read.generation,
                        Packet::Error {
                            token: read.host_token,
                            failure: Failure::Timeout,
                        },
                    );
                }
            }
        }
        let (generation, request) = access(|m| m.request.take().map(|r| (m.generation, r)))?;
        match request {
            Packet::Status { token } => deliver(
                generation,
                Packet::Ready {
                    token,
                    address: local,
                    founder: network.founder,
                    channel: network.channel,
                },
            ),
            Packet::ReadLinks {
                token,
                owner,
                after,
                via,
            } => {
                if owner == 0
                    || owner == u64::MAX
                    || after == u64::MAX
                    || via == u64::MAX
                    || (via != 0 && (via == local || via == owner))
                {
                    deliver(
                        generation,
                        Packet::Error {
                            token,
                            failure: Failure::InvalidRequest,
                        },
                    );
                } else if owner == local && via == 0 {
                    let (revision, link) = forwarder.local_links(after);
                    deliver(
                        generation,
                        Packet::Link {
                            token,
                            owner,
                            after,
                            via,
                            revision,
                            peer: link.map_or(0, |l| l.peer),
                            round_trip_ms: link.map_or(0, |l| l.round_trip_ms),
                        },
                    );
                } else {
                    self.next_token = self.next_token.wrapping_add(1);
                    let read = LinkRead {
                        reader: local,
                        owner,
                        request: self.next_token,
                        after,
                        via,
                    };
                    if let Some(outgoing) = forwarder.originate_links(read, now) {
                        self.read = Some(PendingRead {
                            generation,
                            host_token: token,
                            read,
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
            }
            _ => unreachable!("submit rejects replies"),
        }
        None
    }
    pub(super) fn wait_ms(&self, now: u32) -> Option<u32> {
        self.read
            .as_ref()
            .map(|r| READ_TIMEOUT_MS.saturating_sub(now.wrapping_sub(r.started)))
    }
    pub(super) fn transmit_failed(&mut self, forwarder: &mut Forwarder) {
        if let Some(read) = self.read.take() {
            forwarder.cancel_links(read.read);
            deliver(
                read.generation,
                Packet::Error {
                    token: read.host_token,
                    failure: Failure::Transmit,
                },
            );
        }
    }
    pub(super) fn complete(&mut self, forwarder: &mut Forwarder, incoming: Message<'_>, now: u32) {
        let Some((read, revision, link)) = forwarder.complete_links(incoming, now) else {
            return;
        };
        if !self
            .read
            .as_ref()
            .is_some_and(|pending| pending.read == read)
        {
            return;
        }
        let pending = self.read.take().unwrap();
        deliver(
            pending.generation,
            Packet::Link {
                token: pending.host_token,
                owner: read.owner,
                after: read.after,
                via: read.via,
                revision,
                peer: link.map_or(0, |l| l.peer),
                round_trip_ms: link.map_or(0, |l| l.round_trip_ms),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::peer::frame::Content;
    fn delivered(message: Message<'_>, recipient: u64) -> Message<'static> {
        // Link messages borrow no payload; reconstruct after exercising the real MAC codec.
        let mut bytes = [0; crate::kernel::radio::MAX_FRAME_SIZE];
        let n = message.encode(1, &mut bytes);
        let decoded = Message::decode(&bytes[..n], recipient, 15).unwrap();
        let content = match decoded.content {
            Content::ReadLinks {
                network,
                read,
                remaining,
            } => Content::ReadLinks {
                network,
                read,
                remaining,
            },
            Content::LinkEntry {
                network,
                read,
                revision,
                link,
                hops,
            } => Content::LinkEntry {
                network,
                read,
                revision,
                link,
                hops,
            },
            _ => panic!("unexpected message"),
        };
        Message {
            source: decoded.source,
            destination: decoded.destination,
            content,
        }
    }
    #[test]
    fn local_host_reads_use_real_routes_and_disconnect_cannot_reuse_a_completion() {
        crate::port::test_port_reset();
        access(|m| *m = Mailbox::new());
        let context = unsafe { TaskContext::assume() };
        let network = Network {
            founder: 1,
            channel: 15,
        };
        let mut local = Forwarder::new(1, network);
        let mut remote = Forwarder::new(2, network);
        local.record_presence(2, 0);
        let offer = local.maintain_links(1).unwrap();
        let accept = remote.receive_link(offer, 2).unwrap();
        let confirm = local.receive_link(accept, 3).unwrap();
        remote.receive_link(confirm, 4);
        let mut host = Host::new();
        submit(&context, Packet::Status { token: 1 }).unwrap();
        assert_eq!(
            submit(&context, Packet::Status { token: 2 }),
            Err(Failure::Busy)
        );
        assert!(host.poll(&mut local, 1, network, 5).is_none());
        assert_eq!(
            receive(&context),
            Some(Packet::Ready {
                token: 1,
                address: 1,
                founder: 1,
                channel: 15
            })
        );
        submit(
            &context,
            Packet::ReadLinks {
                token: 2,
                owner: 1,
                after: 0,
                via: 0,
            },
        )
        .unwrap();
        assert!(host.poll(&mut local, 1, network, 6).is_none());
        assert!(matches!(
            receive(&context),
            Some(Packet::Link {
                owner: 1,
                peer: 2,
                ..
            })
        ));
        let request = Packet::ReadLinks {
            token: 77,
            owner: 2,
            after: 0,
            via: 0,
        };
        submit(&context, request).unwrap();
        let outgoing = host.poll(&mut local, 1, network, 7).unwrap();
        assert_eq!(outgoing.destination, Some(2));
        let reply = remote.receive_links(delivered(outgoing, 2), 8).unwrap();
        let mut wrong = reply;
        if let Content::LinkEntry { ref mut read, .. } = wrong.content {
            read.request = read.request.wrapping_add(1);
        }
        host.complete(&mut local, delivered(wrong, 1), 9);
        assert_eq!(receive(&context), None);
        // Closing USB cancels only this host's operation, without forgetting peers.
        disconnect(&context);
        submit(&context, request).unwrap();
        let fresh = host.poll(&mut local, 1, network, 10).unwrap();
        assert_ne!(
            fresh.content, outgoing.content,
            "air token must not reset on USB reconnect"
        );
        host.complete(&mut local, delivered(reply, 1), 11);
        assert_eq!(receive(&context), None);
        let reply = remote.receive_links(delivered(fresh, 2), 12).unwrap();
        host.complete(&mut local, delivered(reply, 1), 13);
        assert!(matches!(
            receive(&context),
            Some(Packet::Link {
                token: 77,
                owner: 2,
                peer: 1,
                ..
            })
        ));
        host.complete(&mut local, delivered(reply, 1), 14);
        assert_eq!(receive(&context), None);
        submit(&context, request).unwrap();
        assert!(host.poll(&mut local, 1, network, 15).is_some());
        assert!(host
            .poll(&mut local, 1, network, 15 + READ_TIMEOUT_MS)
            .is_none());
        assert_eq!(
            receive(&context),
            Some(Packet::Error {
                token: 77,
                failure: Failure::Timeout
            })
        );
        // A reference advertised by the adjacent peer remains traversable
        // through that peer, with the completion returning to this local host.
        let mut third = Forwarder::new(3, network);
        remote.record_presence(3, 8_020);
        let offer = remote.maintain_links(8_021).unwrap();
        let accept = third.receive_link(offer, 8_022).unwrap();
        let confirm = remote.receive_link(accept, 8_023).unwrap();
        third.receive_link(confirm, 8_024);
        submit(
            &context,
            Packet::ReadLinks {
                token: 78,
                owner: 3,
                after: 0,
                via: 2,
            },
        )
        .unwrap();
        let first = host.poll(&mut local, 1, network, 8_025).unwrap();
        assert_eq!(first.destination, Some(2));
        let onward = remote.receive_links(delivered(first, 2), 8_026).unwrap();
        assert_eq!(onward.destination, Some(3));
        let reply = third.receive_links(delivered(onward, 3), 8_027).unwrap();
        let reply = remote.receive_links(delivered(reply, 2), 8_028).unwrap();
        host.complete(&mut local, delivered(reply, 1), 8_029);
        assert!(matches!(
            receive(&context),
            Some(Packet::Link {
                token: 78,
                owner: 3,
                via: 2,
                peer: 2,
                ..
            })
        ));
        for via in [1, 3, u64::MAX] {
            submit(
                &context,
                Packet::ReadLinks {
                    token: 79,
                    owner: 3,
                    after: 0,
                    via,
                },
            )
            .unwrap();
            assert!(host.poll(&mut local, 1, network, 8_030).is_none());
            assert_eq!(
                receive(&context),
                Some(Packet::Error {
                    token: 79,
                    failure: Failure::InvalidRequest
                })
            );
        }
        disconnect(&context);
    }
}
