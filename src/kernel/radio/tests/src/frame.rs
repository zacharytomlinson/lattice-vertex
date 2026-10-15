//! Diagnostic payload over standard 802.15.4 MAC frames, without PHY/FCS bytes.
//! Extended source addresses avoid requiring an assigned short address at boot.

use super::MAX_FRAME_SIZE;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct InvalidFrame;
use byte::BytesExt;
use ieee802154::mac::{
    Address, ExtendedAddress, FooterMode, Frame, FrameContent, FrameSerDesContext, FrameType,
    FrameVersion, Header, PanId, ShortAddress,
};

const PAN: PanId = PanId(0xffff);
const MAGIC: &[u8; 4] = b"LVD\x01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Probe = 0,
    Reply = 1,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Message {
    pub source: u64,
    pub destination: Option<u64>,
    pub kind: Kind,
    pub token: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: u64 = 0x0102_0304_0506_0708;
    const B: u64 = 0x1112_1314_1516_1718;

    #[test]
    fn diagnostic_frames_match_mac_wire_vectors_and_peer_addressing() {
        let cases: &[(Message, &[u8])] = &[
            (
                Message {
                    source: A,
                    destination: None,
                    kind: Kind::Probe,
                    token: 42,
                },
                &[
                    0x41, 0xd8, 0x2a, 0xff, 0xff, 0xff, 0xff, 8, 7, 6, 5, 4, 3, 2, 1, b'L', b'V',
                    b'D', 1, 0, 42, 0, 0, 0,
                ],
            ),
            (
                Message {
                    source: A,
                    destination: Some(B),
                    kind: Kind::Reply,
                    token: 42,
                },
                &[
                    0x41, 0xdc, 0x2a, 0xff, 0xff, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
                    8, 7, 6, 5, 4, 3, 2, 1, b'L', b'V', b'D', 1, 1, 42, 0, 0, 0,
                ],
            ),
        ];
        for (message, wire) in cases {
            let mut out = [0x55; MAX_FRAME_SIZE];
            let n = message.encode(42, &mut out);
            assert_eq!(&out[..n], *wire);
            assert_eq!(out[n], 0x55);
            assert_eq!(&Message::decode(wire, B).unwrap(), message);
            assert!(
                Message::decode(wire, A).is_err(),
                "self-originating frame accepted"
            );
            if message.destination.is_some() {
                assert!(Message::decode(wire, B + 1).is_err());
            }
        }
    }

    #[test]
    fn malformed_or_foreign_frames_cannot_trigger_a_diagnostic_reply() {
        let message = Message {
            source: A,
            destination: None,
            kind: Kind::Probe,
            token: 42,
        };
        let mut out = [0; MAX_FRAME_SIZE];
        let n = message.encode(42, &mut out);
        for end in 0..n {
            assert!(Message::decode(&out[..end], B).is_err(), "prefix {end}");
        }
        // Wrong PAN, MAC ACK request, unsupported version, payload family,
        // unknown operation, and broadcast reply must all be rejected.
        for (offset, value) in [(3, 0), (0, 0x61), (1, 0xe8), (15, b'X'), (19, 2), (19, 1)] {
            let mut changed = out;
            changed[offset] = value;
            assert!(
                Message::decode(&changed[..n], B).is_err(),
                "mutation {offset}:{value}"
            );
        }
        assert!(Message::decode(&out[..n + 1], B).is_err());
        assert!(Message::decode(&[0; MAX_FRAME_SIZE + 1], B).is_err());
    }
}

impl Message {
    pub fn encode(&self, sequence: u8, out: &mut [u8; MAX_FRAME_SIZE]) -> usize {
        let mut payload = [0; 9];
        payload[..4].copy_from_slice(MAGIC);
        payload[4] = self.kind as u8;
        payload[5..].copy_from_slice(&self.token.to_le_bytes());
        let destination = match self.destination {
            Some(address) => Address::Extended(PAN, ExtendedAddress(address)),
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
            payload: &payload,
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

    pub fn decode(bytes: &[u8], local: u64) -> Result<Self, InvalidFrame> {
        if bytes.len() > MAX_FRAME_SIZE {
            return Err(InvalidFrame);
        }
        let frame: Frame = bytes
            .read_with(&mut 0, FooterMode::None)
            .map_err(|_| InvalidFrame)?;
        let h = frame.header;
        if h.frame_type != FrameType::Data
            || h.ack_request
            || h.auxiliary_security_header.is_some()
            || h.ie_present
            || h.seq_no_suppress
            || !h.pan_id_compress
            || h.version != FrameVersion::Ieee802154_2006
        {
            return Err(InvalidFrame);
        }
        let source = match h.source {
            Some(Address::Extended(PAN, ExtendedAddress(a))) if a != local => a,
            _ => return Err(InvalidFrame),
        };
        let destination = match h.destination {
            Some(Address::Extended(PAN, ExtendedAddress(a))) if a == local => Some(a),
            Some(Address::Short(PAN, ShortAddress(0xffff))) => None,
            _ => return Err(InvalidFrame),
        };
        let p = frame.payload;
        if p.len() != 9 || &p[..4] != MAGIC {
            return Err(InvalidFrame);
        }
        let kind = match (p[4], destination) {
            (0, _) => Kind::Probe,
            (1, Some(_)) => Kind::Reply,
            _ => return Err(InvalidFrame),
        };
        Ok(Self {
            source,
            destination,
            kind,
            token: u32::from_le_bytes(p[5..9].try_into().unwrap()),
        })
    }
}
