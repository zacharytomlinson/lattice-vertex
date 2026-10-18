//! Local host messages, independent of USB, the RTOS, and the radio wire format.
//! Versioned little-endian records use COBS and a zero delimiter. USB CDC is a
//! stream: callers feed every byte, regardless of read or endpoint boundaries.

pub const MAX_PACKET: usize = 128;
use crate::scalar::{decode_value, encode_arguments, encode_value, valid_arguments};
use crate::{
    InvokeStatus, StateChange, StateReadResult, Value, CALL_ARGUMENT_BYTES, SURFACE_BYTES,
};
const VERSION: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Failure {
    Busy = 1,
    InvalidRequest = 2,
    Transmit = 3,
    Timeout = 4,
}

/// Fixed communication bytes. Only the initialized prefix is transmitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bytes<const N: usize> {
    bytes: [u8; N],
    len: u8,
}
impl<const N: usize> Bytes<N> {
    pub fn new(data: &[u8]) -> Option<Self> {
        if data.len() > N || data.len() > u8::MAX as usize {
            return None;
        }
        let mut result = Self {
            bytes: [0; N],
            len: data.len() as u8,
        };
        result.bytes[..data.len()].copy_from_slice(data);
        Some(result)
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arguments(Bytes<CALL_ARGUMENT_BYTES>);
impl Arguments {
    pub fn new(values: &[Value]) -> Option<Self> {
        let mut bytes = [0; CALL_ARGUMENT_BYTES];
        let n = encode_arguments(values, &mut bytes)?;
        Some(Self(Bytes::new(&bytes[..n])?))
    }
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        valid_arguments(bytes).then(|| Self(Bytes::new(bytes).unwrap()))
    }
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet {
    UpdateControl {
        token: u32,
        action: crate::update::Control,
    },
    UpdateControlResult {
        token: u32,
        progress: crate::update::Progress,
        release: u64,
    },
    Update {
        token: u32,
        owner: u64,
        data: Bytes<{ crate::update::REQUEST_BYTES }>,
    },
    Updated {
        token: u32,
        owner: u64,
        progress: crate::update::Progress,
        cost: crate::cost::TimePerByte,
    },
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
        cost: crate::cost::TimePerByte,
    },
    ReadSurface {
        token: u32,
        owner: u64,
        offset: u16,
    },
    Surface {
        token: u32,
        owner: u64,
        offset: u16,
        total: u16,
        data: Bytes<SURFACE_BYTES>,
    },
    ReadState {
        token: u32,
        owner: u64,
        attribute: u64,
    },
    State {
        token: u32,
        owner: u64,
        attribute: u64,
        result: StateReadResult,
    },
    Invoke {
        token: u32,
        owner: u64,
        transformer: u64,
        arguments: Arguments,
    },
    Invoked {
        token: u32,
        owner: u64,
        transformer: u64,
        status: InvokeStatus,
    },
    /// Unsolicited registered state, never inferred from invocation success.
    Changed {
        change: StateChange,
    },
    /// Oldest events evicted while the local host was attached but not draining.
    ChangesLost {
        dropped: u32,
    },
    Error {
        token: u32,
        failure: Failure,
    },
}
impl Packet {
    pub fn token(self) -> u32 {
        match self {
            Self::Changed { .. } | Self::ChangesLost { .. } => 0,
            Self::Status { token }
            | Self::UpdateControl { token, .. }
            | Self::UpdateControlResult { token, .. }
            | Self::Update { token, .. }
            | Self::Updated { token, .. }
            | Self::ReadLinks { token, .. }
            | Self::Ready { token, .. }
            | Self::Link { token, .. }
            | Self::ReadSurface { token, .. }
            | Self::Surface { token, .. }
            | Self::ReadState { token, .. }
            | Self::State { token, .. }
            | Self::Invoke { token, .. }
            | Self::Invoked { token, .. }
            | Self::Error { token, .. } => token,
        }
    }
    pub fn is_request(self) -> bool {
        matches!(
            self,
            Self::Status { .. }
                | Self::UpdateControl { .. }
                | Self::Update { .. }
                | Self::ReadLinks { .. }
                | Self::ReadSurface { .. }
                | Self::ReadState { .. }
                | Self::Invoke { .. }
        )
    }

    pub fn encode(self, output: &mut [u8; MAX_PACKET]) -> usize {
        let mut body = [0u8; MAX_PACKET - 2];
        body[0] = VERSION;
        body[2..6].copy_from_slice(&self.token().to_le_bytes());
        let len = match self {
            Self::UpdateControl { action, .. } => {
                body[1] = 6;
                body[6] = action as u8;
                7
            }
            Self::UpdateControlResult {
                progress, release, ..
            } => {
                body[1] = 0x86;
                let mut bytes = [0; crate::update::PROGRESS_BYTES];
                progress.encode(&mut bytes);
                body[6..34].copy_from_slice(&bytes);
                body[34..42].copy_from_slice(&release.to_le_bytes());
                42
            }
            Self::Update { owner, data, .. } => {
                body[1] = 5;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..14 + data.as_slice().len()].copy_from_slice(data.as_slice());
                14 + data.as_slice().len()
            }
            Self::Updated {
                owner,
                progress,
                cost,
                ..
            } => {
                body[1] = 0x85;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                let mut bytes = [0; crate::update::PROGRESS_BYTES];
                progress.encode(&mut bytes);
                body[14..42].copy_from_slice(&bytes);
                body[42..46].copy_from_slice(&cost.raw().to_le_bytes());
                46
            }
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
                cost,
                ..
            } => {
                body[1] = 0x81;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..22].copy_from_slice(&after.to_le_bytes());
                body[22..30].copy_from_slice(&via.to_le_bytes());
                body[30..34].copy_from_slice(&revision.to_le_bytes());
                body[34..42].copy_from_slice(&peer.to_le_bytes());
                body[42..44].copy_from_slice(&round_trip_ms.to_le_bytes());
                body[44..48].copy_from_slice(&cost.raw().to_le_bytes());
                48
            }
            Self::ReadSurface { owner, offset, .. } => {
                body[1] = 2;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..16].copy_from_slice(&offset.to_le_bytes());
                16
            }
            Self::Surface {
                owner,
                offset,
                total,
                data,
                ..
            } => {
                body[1] = 0x82;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..16].copy_from_slice(&offset.to_le_bytes());
                body[16..18].copy_from_slice(&total.to_le_bytes());
                let data = data.as_slice();
                body[18..18 + data.len()].copy_from_slice(data);
                18 + data.len()
            }
            Self::ReadState {
                owner, attribute, ..
            } => {
                body[1] = 3;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..22].copy_from_slice(&attribute.to_le_bytes());
                22
            }
            Self::State {
                owner,
                attribute,
                result,
                ..
            } => {
                body[1] = 0x83;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..22].copy_from_slice(&attribute.to_le_bytes());
                match result {
                    StateReadResult::UnknownAttribute => {
                        body[22] = 1;
                        23
                    }
                    StateReadResult::Busy => {
                        body[22] = 2;
                        23
                    }
                    StateReadResult::Value { sequence, value } => {
                        body[22] = 0;
                        body[23..27].copy_from_slice(&sequence.to_le_bytes());
                        if let Some(value) = value {
                            27 + encode_value(value, &mut body[27..])
                        } else {
                            body[27] = 255;
                            28
                        }
                    }
                }
            }
            Self::Invoke {
                owner,
                transformer,
                arguments,
                ..
            } => {
                body[1] = 4;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..22].copy_from_slice(&transformer.to_le_bytes());
                let args = arguments.as_slice();
                body[22..22 + args.len()].copy_from_slice(args);
                22 + args.len()
            }
            Self::Invoked {
                owner,
                transformer,
                status,
                ..
            } => {
                body[1] = 0x84;
                body[6..14].copy_from_slice(&owner.to_le_bytes());
                body[14..22].copy_from_slice(&transformer.to_le_bytes());
                body[22] = status as u8;
                23
            }
            Self::Changed { change } => {
                body[1] = 0x90;
                body[6..14].copy_from_slice(&change.owner.to_le_bytes());
                body[14..22].copy_from_slice(&change.attribute.to_le_bytes());
                body[22..26].copy_from_slice(&change.sequence.to_le_bytes());
                if let Some(value) = change.value {
                    26 + encode_value(value, &mut body[26..])
                } else {
                    body[26] = 255;
                    27
                }
            }
            Self::ChangesLost { dropped } => {
                body[1] = 0x91;
                body[6..10].copy_from_slice(&dropped.to_le_bytes());
                10
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
        (6, 7) => Packet::UpdateControl {
            token,
            action: crate::update::Control::decode(b[6])?,
        },
        (0x86, 42) => Packet::UpdateControlResult {
            token,
            progress: crate::update::Progress::decode(&b[6..34])?,
            release: value(34),
        },
        (5, 31..=85) => {
            crate::update::Request::decode(&b[14..])?;
            Packet::Update {
                token,
                owner: value(6),
                data: Bytes::new(&b[14..])?,
            }
        }
        (0x85, 46) => Packet::Updated {
            token,
            owner: value(6),
            progress: crate::update::Progress::decode(&b[14..42])?,
            cost: crate::cost::TimePerByte::from_raw(u32::from_le_bytes(
                b[42..46].try_into().ok()?,
            )),
        },
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
        (0x81, 48) => Packet::Link {
            token,
            owner: value(6),
            after: value(14),
            via: value(22),
            revision: u32::from_le_bytes(b[30..34].try_into().ok()?),
            peer: value(34),
            round_trip_ms: u16::from_le_bytes(b[42..44].try_into().ok()?),
            cost: crate::cost::TimePerByte::from_raw(u32::from_le_bytes(
                b[44..48].try_into().ok()?,
            )),
        },
        (2, 16) => Packet::ReadSurface {
            token,
            owner: value(6),
            offset: u16::from_le_bytes(b[14..16].try_into().ok()?),
        },
        (0x82, 18..=85) => {
            let offset = u16::from_le_bytes(b[14..16].try_into().ok()?);
            let total = u16::from_le_bytes(b[16..18].try_into().ok()?);
            if total < 13
                || offset > total
                || b.len() - 18 != SURFACE_BYTES.min((total - offset) as usize)
            {
                return None;
            }
            Packet::Surface {
                token,
                owner: value(6),
                offset,
                total,
                data: Bytes::new(&b[18..])?,
            }
        }
        (3, 22) => Packet::ReadState {
            token,
            owner: value(6),
            attribute: value(14),
        },
        (0x83, 23..=36) => {
            let result = match (b[22], b.len()) {
                (1, 23) => StateReadResult::UnknownAttribute,
                (2, 23) => StateReadResult::Busy,
                (0, 28..=36) => {
                    let sequence = u32::from_le_bytes(b[23..27].try_into().ok()?);
                    let value = if b[27] == 255 {
                        if b.len() != 28 {
                            return None;
                        }
                        None
                    } else {
                        let (v, n) = decode_value(&b[27..])?;
                        if b.len() != 27 + n {
                            return None;
                        }
                        Some(v)
                    };
                    StateReadResult::Value { sequence, value }
                }
                _ => return None,
            };
            Packet::State {
                token,
                owner: value(6),
                attribute: value(14),
                result,
            }
        }
        (4, 23..=83) => Packet::Invoke {
            token,
            owner: value(6),
            transformer: value(14),
            arguments: Arguments::from_bytes(&b[22..])?,
        },
        (0x84, 23) => Packet::Invoked {
            token,
            owner: value(6),
            transformer: value(14),
            status: match b[22] {
                0 => InvokeStatus::Invoked,
                1 => InvokeStatus::Arguments,
                2 => InvokeStatus::UnknownTransformer,
                3 => InvokeStatus::ApplianceError,
                4 => InvokeStatus::Busy,
                5 => InvokeStatus::ConflictingRequest,
                _ => return None,
            },
        },
        (0x90, 27..=35) if token == 0 => {
            let state = if b[26] == 255 {
                if b.len() != 27 {
                    return None;
                }
                None
            } else {
                let (v, n) = decode_value(&b[26..])?;
                if b.len() != 26 + n {
                    return None;
                }
                Some(v)
            };
            Packet::Changed {
                change: StateChange {
                    owner: value(6),
                    attribute: value(14),
                    sequence: u32::from_le_bytes(b[22..26].try_into().ok()?),
                    value: state,
                },
            }
        }
        (0x91, 10) if token == 0 => Packet::ChangesLost {
            dropped: u32::from_le_bytes(b[6..10].try_into().ok()?),
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
        let mut update = [0; crate::update::REQUEST_BYTES];
        let n = crate::update::Request::Data {
            package: [5; 16],
            offset: u32::MAX,
            data: &[255; crate::update::DATA_BYTES],
        }
        .encode(&mut update);
        let records = [
            Packet::Update {
                token: 9,
                owner: 19,
                data: Bytes::new(&update[..n]).unwrap(),
            },
            Packet::Updated {
                token: 9,
                owner: 19,
                progress: crate::update::Progress::unsupported([5; 16]),
                cost: crate::cost::TimePerByte::UNKNOWN,
            },
            Packet::UpdateControl {
                token: 10,
                action: crate::update::Control::SelectTrial,
            },
            Packet::UpdateControlResult {
                token: 10,
                progress: crate::update::Progress::unsupported([5; 16]),
                release: u64::MAX,
            },
            Packet::Status { token: 0 },
            Packet::Changed {
                change: StateChange {
                    owner: 19,
                    attribute: 123,
                    sequence: u32::MAX,
                    value: Some(Value::integer(crate::Integer::I64, i64::MIN as i128).unwrap()),
                },
            },
            Packet::Changed {
                change: StateChange {
                    owner: 19,
                    attribute: 123,
                    sequence: 0,
                    value: None,
                },
            },
            Packet::ChangesLost { dropped: u32::MAX },
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
                cost: crate::cost::TimePerByte::measured(1000, 20),
            },
            Packet::ReadSurface {
                token: 5,
                owner: 19,
                offset: 0,
            },
            Packet::Surface {
                token: 6,
                owner: 19,
                offset: 0,
                total: 90,
                data: Bytes::new(&[255; SURFACE_BYTES]).unwrap(),
            },
            Packet::ReadState {
                token: 7,
                owner: 19,
                attribute: 123,
            },
            Packet::State {
                token: 7,
                owner: 19,
                attribute: 123,
                result: StateReadResult::Value {
                    sequence: 7,
                    value: Some(Value::integer(crate::Integer::I64, -1000).unwrap()),
                },
            },
            Packet::Invoke {
                token: 8,
                owner: 19,
                transformer: 234,
                arguments: Arguments::new(&[Value::boolean(true); 30]).unwrap(),
            },
            Packet::Invoked {
                token: 8,
                owner: 19,
                transformer: 234,
                status: InvokeStatus::Invoked,
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
        assert_eq!(decode_body(&[1, 0, 0, 0, 0, 0]), None);
        assert_eq!(decode_body(&[2, 0, 0, 0, 0, 0]), None);
        assert_eq!(decode_body(&[3, 0, 0, 0, 0, 0]), None);
        let mut change = [0; 28];
        change[0] = VERSION;
        change[1] = 0x90;
        change[26] = 255;
        assert!(decode_body(&change[..27]).is_some());
        assert_eq!(
            decode_body(&change),
            None,
            "unavailable cannot carry trailing bytes"
        );
        change[2] = 1;
        assert_eq!(
            decode_body(&change[..27]),
            None,
            "events require reserved token zero"
        );
        assert_eq!(decode_body(&[1, 0, 0, 0, 0, 0, 0]), None);
    }
}
