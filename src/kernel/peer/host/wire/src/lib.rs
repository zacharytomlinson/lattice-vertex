//! Local host messages, independent of USB, the RTOS, and the radio wire format.
//! Versioned little-endian records use COBS and a zero delimiter. USB CDC is a
//! stream: callers feed every byte, regardless of read or endpoint boundaries.
#![no_std]

pub const MAX_PACKET: usize = 64;
const VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Failure {
    Busy = 1,
    InvalidRequest = 2,
    Transmit = 3,
    Timeout = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet {
    Status {
        token: u32,
    },
    ReadLinks {
        token: u32,
        owner: u64,
        after: u64,
        via: u64,
    },
    Ready {
        token: u32,
        address: u64,
        founder: u64,
        channel: u8,
    },
    /// Peer zero terminates the cursor. Revision changes invalidate an enumeration.
    Link {
        token: u32,
        owner: u64,
        after: u64,
        via: u64,
        revision: u32,
        peer: u64,
        round_trip_ms: u16,
    },
    Error {
        token: u32,
        failure: Failure,
    },
}
impl Packet {
    pub fn token(self) -> u32 {
        match self {
            Self::Status { token }
            | Self::ReadLinks { token, .. }
            | Self::Ready { token, .. }
            | Self::Link { token, .. }
            | Self::Error { token, .. } => token,
        }
    }
    pub fn is_request(self) -> bool {
        matches!(self, Self::Status { .. } | Self::ReadLinks { .. })
    }

    pub fn encode(self, output: &mut [u8; MAX_PACKET]) -> usize {
        let mut body = [0u8; 44];
        body[0] = VERSION;
        body[2..6].copy_from_slice(&self.token().to_le_bytes());
        let len = match self {
            Self::Status { .. } => {
                body[1] = 0;
                6
            }
            Self::ReadLinks {
                owner, after, via, ..
            } => {
                body[1] = 1;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..22].copy_from_slice(&after.to_le_bytes());
                body[22..30].copy_from_slice(&via.to_le_bytes());
                30
            }
            Self::Ready {
                address,
                founder,
                channel,
                ..
            } => {
                body[1] = 0x80;
                body[6..14].copy_from_slice(&address.to_le_bytes());
                body[14..22].copy_from_slice(&founder.to_le_bytes());
                body[22] = channel;
                23
            }
            Self::Link {
                owner,
                after,
                via,
                revision,
                peer,
                round_trip_ms,
                ..
            } => {
                body[1] = 0x81;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..22].copy_from_slice(&after.to_le_bytes());
                body[22..30].copy_from_slice(&via.to_le_bytes());
                body[30..34].copy_from_slice(&revision.to_le_bytes());
                body[34..42].copy_from_slice(&peer.to_le_bytes());
                body[42..44].copy_from_slice(&round_trip_ms.to_le_bytes());
                44
            }
            Self::Error { failure, .. } => {
                body[1] = 0xff;
                body[6] = failure as u8;
                7
            }
        };
        // Records are below 254 bytes, so each COBS block fits in one code byte.
        let (mut code_index, mut code, mut end) = (0, 1u8, 1);
        for byte in &body[..len] {
            if *byte == 0 {
                output[code_index] = code;
                code_index = end;
                end += 1;
                code = 1;
            } else {
                output[end] = *byte;
                end += 1;
                code += 1;
            }
        }
        output[code_index] = code;
        output[end] = 0;
        end + 1
    }
}

/// Bounded stream decoder. Invalid/oversized records are discarded through their
/// delimiter, so they cannot turn a suffix into a different request.
pub struct Decoder {
    bytes: [u8; MAX_PACKET],
    len: usize,
    overflow: bool,
}
impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}
impl Decoder {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_PACKET],
            len: 0,
            overflow: false,
        }
    }
    pub fn push(&mut self, byte: u8) -> Option<Packet> {
        if byte != 0 {
            if self.len == self.bytes.len() {
                self.overflow = true;
            } else {
                self.bytes[self.len] = byte;
                self.len += 1;
            }
            return None;
        }
        let len = self.len;
        self.len = 0;
        let overflow = core::mem::replace(&mut self.overflow, false);
        if overflow || len == 0 {
            return None;
        }
        let mut body = [0u8; MAX_PACKET];
        let (mut input, mut end) = (0, 0);
        while input < len {
            let code = self.bytes[input] as usize;
            input += 1;
            let count = code.checked_sub(1)?;
            if input + count > len || end + count >= body.len() {
                return None;
            }
            body[end..end + count].copy_from_slice(&self.bytes[input..input + count]);
            input += count;
            end += count;
            if input < len {
                body[end] = 0;
                end += 1;
            }
        }
        decode_body(&body[..end])
    }
}
fn decode_body(b: &[u8]) -> Option<Packet> {
    if b.len() < 6 || b[0] != VERSION {
        return None;
    }
    let token = u32::from_le_bytes(b[2..6].try_into().ok()?);
    let value = |at| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    Some(match (b[1], b.len()) {
        (0, 6) => Packet::Status { token },
        (1, 30) => Packet::ReadLinks {
            token,
            owner: value(6),
            after: value(14),
            via: value(22),
        },
        (0x80, 23) => Packet::Ready {
            token,
            address: value(6),
            founder: value(14),
            channel: b[22],
        },
        (0x81, 44) => Packet::Link {
            token,
            owner: value(6),
            after: value(14),
            via: value(22),
            revision: u32::from_le_bytes(b[30..34].try_into().ok()?),
            peer: value(34),
            round_trip_ms: u16::from_le_bytes(b[42..44].try_into().ok()?),
        },
        (0xff, 7) => Packet::Error {
            token,
            failure: match b[6] {
                1 => Failure::Busy,
                2 => Failure::InvalidRequest,
                3 => Failure::Transmit,
                4 => Failure::Timeout,
                _ => return None,
            },
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_boundaries_and_invalid_records_do_not_change_messages() {
        let records = [
            Packet::Status { token: 0 },
            Packet::ReadLinks {
                token: u32::MAX,
                owner: 0x0100020003000400,
                after: 0,
                via: 42,
            },
            Packet::Ready {
                token: 3,
                address: 17,
                founder: 18,
                channel: 26,
            },
            Packet::Link {
                token: 4,
                owner: 17,
                after: 9,
                via: 0,
                revision: 18,
                peer: u64::MAX,
                round_trip_ms: u16::MAX,
            },
            Packet::Error {
                token: 5,
                failure: Failure::Timeout,
            },
        ];
        for record in records {
            let mut encoded = [0; MAX_PACKET];
            let n = record.encode(&mut encoded);
            for split in 0..=n {
                let mut decoder = Decoder::new();
                let mut decoded = None;
                for chunk in [&encoded[..split], &encoded[split..n]] {
                    for byte in chunk {
                        if let Some(packet) = decoder.push(*byte) {
                            assert!(decoded.replace(packet).is_none());
                        }
                    }
                }
                assert_eq!(decoded, Some(record));
            }
            let mut decoder = Decoder::new();
            for _ in 0..MAX_PACKET + 10 {
                assert_eq!(decoder.push(1), None);
            }
            assert_eq!(decoder.push(0), None);
            for byte in [99, 2, 0] {
                assert_eq!(decoder.push(byte), None);
            }
            assert_eq!(
                encoded[..n].iter().filter_map(|b| decoder.push(*b)).last(),
                Some(record)
            );
            // Consecutive records need no extra synchronization bytes.
            assert_eq!(
                encoded[..n].iter().filter_map(|b| decoder.push(*b)).last(),
                Some(record)
            );
        }
        assert_eq!(decode_body(&[2, 0, 0, 0, 0, 0]), None);
        assert_eq!(decode_body(&[1, 0, 0, 0, 0, 0, 0]), None);
    }
}
