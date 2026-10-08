//! Common channel formation, discovery, and arrival-triggered surface publication.
//! Residents share the channel choice; the original founder has no special
//! runtime role. This is reachability, not authority to mutate an appliance.

mod bootstrap;
pub(crate) mod exposure;
mod frame;

use frame::{Content, Message};

// Common discovery filter, not an allocated PAN or membership credential.
const PAN_ID: u16 = 0x4c56;

/// Retained origin and channel of this network. Any resident advertises it.
/// The founding address is a tag, not the address of a required live coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Network {
    founder: u64,
    channel: u8,
}

impl Network {
    fn valid(self) -> bool {
        self.founder != 0 && self.founder != u64::MAX && (11..=26).contains(&self.channel)
    }
}

fn discovery_response(
    local: u64,
    incoming: &Message<'_>,
    network: Network,
    resident: bool,
) -> Option<Message<'static>> {
    match (incoming.content, incoming.destination) {
        (Content::Discover | Content::Propose(_), None) => Some(Message {
            source: local,
            destination: Some(incoming.source),
            content: if resident {
                Content::Present(network)
            } else {
                Content::Propose(network)
            },
        }),
        _ => None,
    }
}

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(crate) fn run(
    radio: &mut super::radio::task::Transport,
    publication: exposure::Publication,
) -> ! {
    use super::{radio::MAX_FRAME_SIZE, tasks};
    let local = radio.address();
    let mut buffer = [0; MAX_FRAME_SIZE];
    let mut sequence = 0u8;
    let network = bootstrap::join_or_form(radio, local, &mut sequence, &mut buffer);
    send(
        radio,
        Message {
            source: local,
            destination: None,
            content: Content::Arrive(network),
        },
        &mut sequence,
        &mut buffer,
    );
    let mut publisher = exposure::Publisher::new(publication);
    let publication_period = crate::milliseconds_to_ticks_checked(7 + local % 7).unwrap();
    let mut last_piece = tasks::xTaskGetTickCount();
    let period = crate::milliseconds_to_ticks_checked(2_000 + local % 997).unwrap();
    let mut last_discovery = tasks::xTaskGetTickCount().wrapping_sub(period);
    loop {
        let now = tasks::xTaskGetTickCount();
        if now.wrapping_sub(last_discovery) >= period {
            last_discovery = now;
            send(
                radio,
                Message {
                    source: local,
                    destination: None,
                    content: Content::Discover,
                },
                &mut sequence,
                &mut buffer,
            );
        }
        let now = tasks::xTaskGetTickCount();
        if publisher.has_work() && now.wrapping_sub(last_piece) >= publication_period {
            let mut piece = [0; frame::SURFACE_BYTES];
            let n = publisher.prepare_part(&mut piece);
            if n != 0 {
                let outgoing = Message {
                    source: local,
                    destination: None,
                    content: Content::SurfacePart {
                        network,
                        sequence: publisher.sequence,
                        offset: publisher.offset,
                        total: publisher.publication.len(),
                        bytes: &piece[..n],
                    },
                };
                if send(radio, outgoing, &mut sequence, &mut buffer) {
                    publisher.transmitted(n);
                } else {
                    publisher.transmit_failed();
                }
            }
            last_piece = tasks::xTaskGetTickCount();
        }
        let now = tasks::xTaskGetTickCount();
        let mut timeout = period.saturating_sub(now.wrapping_sub(last_discovery));
        if publisher.has_work() {
            timeout = timeout.min(publication_period.saturating_sub(now.wrapping_sub(last_piece)));
        }
        if let Some(message) = receive(radio, local, network.channel, &mut buffer, timeout) {
            if message.content == Content::Arrive(network) {
                publisher.arrival();
                #[cfg(feature = "test-support")]
                super::radio::testing::event(super::radio::testing::Event::ArrivalReceived);
            } else if let Content::SurfacePart {
                network: advertised,
                sequence,
                offset,
                total,
                bytes,
            } = message.content
            {
                if advertised == network {
                    #[cfg(feature = "test-support")]
                    super::radio::testing::event(
                        super::radio::testing::Event::SurfacePartReceived {
                            source: message.source,
                            sequence,
                            offset,
                            total,
                            bytes,
                        },
                    );
                    // Uninterested vertices retain no foreign definitions. This
                    // borrowed packet ends here; interested consumers own reassembly.
                    let _ = (sequence, offset, total, bytes);
                }
            } else if let Some(reply) = discovery_response(local, &message, network, true) {
                send(radio, reply, &mut sequence, &mut buffer);
            } else if message.content == Content::Present(network) {
                #[cfg(feature = "test-support")]
                super::radio::testing::event(
                    super::radio::testing::Event::DiscoveryResponseReceived(message.source),
                );
            }
        }
    }
}

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
fn send(
    radio: &mut super::radio::task::Transport,
    message: Message<'_>,
    sequence: &mut u8,
    buffer: &mut [u8; super::radio::MAX_FRAME_SIZE],
) -> bool {
    let length = message.encode(*sequence, buffer);
    *sequence = sequence.wrapping_add(1);
    let result = radio.transmit(&buffer[..length]);
    #[cfg(feature = "test-support")]
    {
        use super::radio::testing::{event, Event};
        event(if result.is_err() {
            Event::TransmitFailed
        } else {
            match message.content {
                Content::Discover => Event::DiscoveryRequestSent,
                Content::Present(_) => Event::DiscoveryResponseSent,
                Content::Propose(_) => Event::ChannelProposalSent,
                Content::Arrive(_) => Event::ArrivalSent,
                Content::SurfacePart {
                    sequence,
                    offset,
                    total,
                    bytes,
                    ..
                } => Event::SurfacePartSent {
                    sequence,
                    offset,
                    total,
                    bytes,
                },
            }
        });
    }
    result.is_ok()
}

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
fn receive<'a>(
    radio: &mut super::radio::task::Transport,
    local: u64,
    channel: u8,
    buffer: &'a mut [u8; super::radio::MAX_FRAME_SIZE],
    timeout: crate::types::TickType_t,
) -> Option<Message<'a>> {
    #[cfg(feature = "test-support")]
    use super::radio::testing::{event, Event};
    match radio.receive(buffer, timeout) {
        Some(Ok(length)) => match Message::decode(&buffer[..length], local, channel) {
            Ok(message) => {
                #[cfg(feature = "test-support")]
                if message.content == Content::Discover {
                    event(Event::DiscoveryRequestReceived(message.source));
                }
                Some(message)
            }
            Err(_) => {
                #[cfg(feature = "test-support")]
                event(Event::FrameIgnored);
                None
            }
        },
        Some(Err(_)) => {
            #[cfg(feature = "test-support")]
            event(Event::ReceiveFailed);
            None
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::radio::MAX_FRAME_SIZE;

    #[test]
    fn every_resident_answers_with_inherited_network_and_presence_never_echoes() {
        let network = Network {
            founder: 99,
            channel: 23,
        };
        for (sender, receiver) in [(1, 2), (2, 1)] {
            for resident in [false, true] {
                let mut wire = [0; MAX_FRAME_SIZE];
                let request = Message {
                    source: sender,
                    destination: None,
                    content: Content::Discover,
                };
                let n = request.encode(255, &mut wire);
                let incoming = Message::decode(&wire[..n], receiver, 23).unwrap();
                let reply = discovery_response(receiver, &incoming, network, resident).unwrap();
                assert_eq!(reply.destination, Some(sender));
                let n = reply.encode(0, &mut wire);
                let received = Message::decode(&wire[..n], sender, 23).unwrap();
                assert_eq!(received.source, receiver);
                assert_eq!(
                    received.content,
                    if resident {
                        Content::Present(network)
                    } else {
                        Content::Propose(network)
                    }
                );
                assert_eq!(discovery_response(sender, &received, network, true), None);
            }
        }
        let proposed = Message {
            source: 1,
            destination: None,
            content: Content::Propose(Network {
                founder: 1,
                channel: 11,
            }),
        };
        let reply = discovery_response(2, &proposed, network, true).unwrap();
        assert_eq!(
            reply.content,
            Content::Present(network),
            "an established resident must answer a candidate with the existing network"
        );
        assert_eq!(reply.destination, Some(1));
        for content in [
            Content::Arrive(network),
            Content::SurfacePart {
                network,
                sequence: 1,
                offset: 0,
                total: 13,
                bytes: &[0; 13],
            },
        ] {
            let message = Message {
                source: 1,
                destination: None,
                content,
            };
            assert!(
                discovery_response(2, &message, network, true).is_none(),
                "surface traffic must not create a discovery-response echo"
            );
        }
    }
}
