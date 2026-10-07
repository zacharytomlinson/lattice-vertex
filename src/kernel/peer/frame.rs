//! Version-2 discovery envelope over standard 802.15.4 data frames.
//! PHY length/FCS belong to the port. Network tags are not credentials.

use super::{Network, PAN_ID};
use crate::kernel::radio::MAX_FRAME_SIZE;
use byte::BytesExt;
use ieee802154::mac::{
    Address, ExtendedAddress, FooterMode, Frame, FrameContent, FrameSerDesContext, FrameType,
    FrameVersion, Header, PanId, ShortAddress,
};

const PAN: PanId = PanId(PAN_ID);
const MAGIC: &[u8; 4] = b"LVT\x02";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Content {
    Discover,
    Present(Network),
    Propose(Network),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Message {
    pub source: u64,
    pub destination: Option<u64>,
    pub content: Content,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct InvalidFrame;

impl Message {
    pub fn encode(&self, sequence: u8, out: &mut [u8; MAX_FRAME_SIZE]) -> usize {
        let mut payload = [0; 14];
        payload[..4].copy_from_slice(MAGIC);
        let size = match self.content {
            Content::Discover => 5,
            Content::Present(network) | Content::Propose(network) => {
                payload[4] = if matches!(self.content, Content::Present(_)) {
                    1
                } else {
                    2
                };
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                14
            }
        };
        let destination = match self.destination {
            Some(a) => Address::Extended(PAN, ExtendedAddress(a)),
            None => Address::Short(PAN, ShortAddress(0xffff)),
        };
        let frame = Frame {
            header: Header {
                frame_type: FrameType::Data,
                frame_pending: false,
                ack_request: false,
                pan_id_compress: true,
                seq_no_suppress: false,
                ie_present: false,
                version: FrameVersion::Ieee802154_2006,
                seq: sequence,
                destination: Some(destination),
                source: Some(Address::Extended(PAN, ExtendedAddress(self.source))),
                auxiliary_security_header: None,
            },
            content: FrameContent::Data,
            payload: &payload[..size],
            footer: [0; 2],
        };
        let mut length = 0;
        out.write_with(
            &mut length,
            frame,
            &mut FrameSerDesContext::no_security(FooterMode::None),
        )
        .unwrap();
        length
    }

    pub fn decode(bytes: &[u8], local: u64, channel: u8) -> Result<Self, InvalidFrame> {
        if bytes.len() > MAX_FRAME_SIZE || bytes.len() < 3 {
            return Err(InvalidFrame);
        }
        // Exact profile: 2006 data, compressed PAN, extended source, short
        // broadcast/extended destination, no ACK/security/IEs/reserved bits.
        if bytes[0] != 0x41 || !matches!(bytes[1], 0xd8 | 0xdc) {
            return Err(InvalidFrame);
        }
        let frame: Frame = bytes
            .read_with(&mut 0, FooterMode::None)
            .map_err(|_| InvalidFrame)?;
        let source = match frame.header.source {
            Some(Address::Extended(PAN, ExtendedAddress(a)))
                if a != local && a != 0 && a != u64::MAX =>
            {
                a
            }
            _ => return Err(InvalidFrame),
        };
        let destination = match frame.header.destination {
            Some(Address::Extended(PAN, ExtendedAddress(a))) if a == local => Some(a),
            Some(Address::Short(PAN, ShortAddress(0xffff))) => None,
            _ => return Err(InvalidFrame),
        };
        let p = frame.payload;
        if p.len() < 5 || &p[..4] != MAGIC {
            return Err(InvalidFrame);
        }
        let content = match (p[4], p.len(), destination) {
            (0, 5, None) => Content::Discover,
            (1 | 2, 14, _) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                // Residents answer on their operating channel. A candidate
                // also advertises its proposed channel while sweeping others.
                if !network.valid()
                    || (p[4] == 1 && (network.channel != channel || destination.is_none()))
                {
                    return Err(InvalidFrame);
                }
                if p[4] == 1 {
                    Content::Present(network)
                } else {
                    Content::Propose(network)
                }
            }
            _ => return Err(InvalidFrame),
        };
        Ok(Self {
            source,
            destination,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: u64 = 0x0102_0304_0506_0708;
    const B: u64 = 0x1112_1314_1516_1718;

    #[test]
    fn wire_vectors_and_rejection_boundaries() {
        let network = Network {
            founder: 0x2122_2324_2526_2728,
            channel: 23,
        };
        let cases: &[(Message, &[u8])] = &[
            (
                Message {
                    source: A,
                    destination: None,
                    content: Content::Discover,
                },
                &[
                    0x41, 0xd8, 42, 0x56, 0x4c, 0xff, 0xff, 8, 7, 6, 5, 4, 3, 2, 1, b'L', b'V',
                    b'T', 2, 0,
                ],
            ),
            (
                Message {
                    source: A,
                    destination: Some(B),
                    content: Content::Present(network),
                },
                &[
                    0x41, 0xdc, 42, 0x56, 0x4c, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 8,
                    7, 6, 5, 4, 3, 2, 1, b'L', b'V', b'T', 2, 1, 0x28, 0x27, 0x26, 0x25, 0x24,
                    0x23, 0x22, 0x21, 23,
                ],
            ),
            (
                Message {
                    source: A,
                    destination: None,
                    content: Content::Propose(network),
                },
                &[
                    0x41, 0xd8, 42, 0x56, 0x4c, 0xff, 0xff, 8, 7, 6, 5, 4, 3, 2, 1, b'L', b'V',
                    b'T', 2, 2, 0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21, 23,
                ],
            ),
        ];
        for (message, wire) in cases {
            let mut out = [0x55; MAX_FRAME_SIZE];
            let n = message.encode(42, &mut out);
            assert_eq!(&out[..n], *wire);
            assert_eq!(out[n], 0x55);
            assert_eq!(&Message::decode(wire, B, 23).unwrap(), message);
            assert!(Message::decode(wire, A, 23).is_err());
            if message.destination.is_some() {
                assert!(Message::decode(wire, B + 1, 23).is_err());
            }
            for end in 0..n {
                assert!(Message::decode(&out[..end], B, 23).is_err(), "prefix {end}");
            }
            assert!(Message::decode(&out[..n + 1], B, 23).is_err());
            let payload = if message.destination.is_some() {
                21
            } else {
                15
            };
            for (offset, value) in [
                (3, 0xff),
                (4, 0xff),
                (0, 0x49),
                (0, 0x61),
                (0, 0xc1),
                (1, 0xe8),
                (1, 0xd9),
                (payload, b'X'),
                (payload + 2, b'D'),
                (payload + 3, 1),
                (payload + 4, 3),
            ] {
                let mut bad = out;
                bad[offset] = value;
                assert!(
                    Message::decode(&bad[..n], B, 23).is_err(),
                    "mutation {offset}:{value}"
                );
            }
            for source in [0u64, u64::MAX] {
                let mut bad = out;
                bad[payload - 8..payload].copy_from_slice(&source.to_le_bytes());
                assert!(Message::decode(&bad[..n], B, 23).is_err());
            }
            if message.content != Content::Discover {
                if matches!(message.content, Content::Present(_)) {
                    assert!(
                        Message::decode(wire, B, 22).is_err(),
                        "stale resident channel"
                    );
                } else {
                    assert_eq!(
                        &Message::decode(wire, B, 22).unwrap(),
                        message,
                        "a scanning candidate must be able to advertise its proposed channel"
                    );
                }
                for founder in [0u64, u64::MAX] {
                    let mut bad = out;
                    bad[payload + 5..payload + 13].copy_from_slice(&founder.to_le_bytes());
                    assert!(Message::decode(&bad[..n], B, 23).is_err());
                }
                for channel in [0, 10, 27, 255] {
                    let mut bad = out;
                    bad[n - 1] = channel;
                    assert!(Message::decode(&bad[..n], B, channel).is_err());
                }
            }
        }
        for content in [Content::Discover, Content::Present(network)] {
            let invalid = Message {
                source: A,
                destination: if content == Content::Discover {
                    Some(B)
                } else {
                    None
                },
                content,
            };
            let mut out = [0; MAX_FRAME_SIZE];
            let n = invalid.encode(0, &mut out);
            assert!(Message::decode(&out[..n], B, 23).is_err());
        }
        assert!(Message::decode(&[0; MAX_FRAME_SIZE + 1], B, 23).is_err());
    }
}
