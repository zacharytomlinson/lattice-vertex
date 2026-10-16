//! Peer links, traversal, surface/state reads, and native control over 802.15.4.
//! PHY length/FCS belong to the port. Network tags are not credentials.

use super::{Network, PAN_ID};
#[cfg(test)]
use crate::ir::{Integer, Value};
use crate::kernel::radio::MAX_FRAME_SIZE;
use byte::BytesExt;
use ieee802154::mac::{
    Address, ExtendedAddress, FooterMode, Frame, FrameContent, FrameSerDesContext, FrameType,
    FrameVersion, Header, PanId, ShortAddress,
};
#[cfg(test)]
pub(super) use lattice_peer_wire::scalar::encode_arguments;
use lattice_peer_wire::scalar::valid_arguments;
pub(super) use lattice_peer_wire::scalar::{
    decode_arguments, decode_value, encode_value, type_tag,
};
pub(crate) use lattice_peer_wire::{InvokeStatus, StateReadResult};

const PAN: PanId = PanId(PAN_ID);
const MAGIC: &[u8; 4] = b"LVT\x06";
pub(super) const MAX_HOPS: u8 = 8;
const MAX_PAYLOAD: usize = MAX_FRAME_SIZE - 21;
// Worst-case addressed MAC header plus the complete routed reply envelope.
pub(super) use lattice_peer_wire::SURFACE_BYTES;
const _: () = assert!(SURFACE_BYTES == MAX_PAYLOAD - 37);
pub(super) use lattice_peer_wire::{CALL_ARGUMENT_BYTES, MAX_CALL_ARGUMENTS};
const _: () = assert!(CALL_ARGUMENT_BYTES == MAX_PAYLOAD - 43);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Invocation {
    pub caller: u64,
    pub owner: u64,
    pub request: u32,
    pub transformer: u64,
}

pub(super) use lattice_peer_wire::StateChange;

/// Presence metadata only; no mathematical definition or live value is copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SurfaceOwner {
    pub address: u64,
    pub surface_bytes: u16,
}

/// One immutable range request. The reader chooses a fresh token for each
/// attempt; MAC sequence numbers identify individual link transmissions only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SurfaceRead {
    pub reader: u64,
    pub owner: u64,
    pub request: u16,
    pub offset: u16,
}

/// Read one exposed attribute. Read tokens share the surface-read namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StateRead {
    pub reader: u64,
    pub owner: u64,
    pub request: u16,
    pub attribute: u64,
}

/// One-hop admission/refresh exchange. These messages are never relayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum LinkPhase {
    Offer = 0,
    Accept = 1,
    Confirm = 2,
    Reject = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PeerLink {
    pub peer: u64,
    pub round_trip_ms: u16,
}

/// Enumerate an owner's resident links after a peer-address cursor. A nonzero
/// via names the already-reachable vertex that advertised this owner as a neighbor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LinkRead {
    pub reader: u64,
    pub owner: u64,
    pub request: u16,
    pub after: u64,
    pub via: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Content<'a> {
    Discover,
    Present(Network),
    Propose(Network),
    Arrive {
        network: Network,
        owner: SurfaceOwner,
        sequence: u16,
        hops: u8,
    },
    FindOwners {
        network: Network,
        seeker: u64,
        request: u16,
        remaining: u8,
    },
    OwnerFound {
        network: Network,
        seeker: u64,
        request: u16,
        owner: SurfaceOwner,
        hops: u8,
    },
    ReadSurface {
        network: Network,
        read: SurfaceRead,
        remaining: u8,
    },
    SurfacePart {
        network: Network,
        read: SurfaceRead,
        total: u16,
        hops: u8,
        bytes: &'a [u8],
    },
    Invoke {
        network: Network,
        call: Invocation,
        remaining: u8,
        copies: u8,
        arguments: &'a [u8],
    },
    InvocationResult {
        network: Network,
        call: Invocation,
        status: InvokeStatus,
        hops: u8,
    },
    StateChanged {
        network: Network,
        delta: StateChange,
        hops: u8,
    },
    ReadState {
        network: Network,
        read: StateRead,
        remaining: u8,
    },
    StateValue {
        network: Network,
        read: StateRead,
        result: StateReadResult,
        hops: u8,
    },
    LinkExchange {
        network: Network,
        phase: LinkPhase,
        token: u32,
    },
    ReadLinks {
        network: Network,
        read: LinkRead,
        remaining: u8,
    },
    LinkEntry {
        network: Network,
        read: LinkRead,
        revision: u32,
        link: Option<PeerLink>,
        hops: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Message<'a> {
    pub source: u64,
    pub destination: Option<u64>,
    pub content: Content<'a>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct InvalidFrame;

impl<'a> Message<'a> {
    pub fn encode(&self, sequence: u8, out: &mut [u8; MAX_FRAME_SIZE]) -> usize {
        let mut payload = [0; 37 + SURFACE_BYTES];
        payload[..4].copy_from_slice(MAGIC);
        let size = match self.content {
            Content::Discover => 5,
            Content::Present(network) | Content::Propose(network) => {
                payload[4] = match self.content {
                    Content::Present(_) => 1,
                    _ => 2,
                };
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                14
            }
            Content::Arrive {
                network,
                owner,
                sequence,
                hops,
            } => {
                payload[4] = 3;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                payload[14..22].copy_from_slice(&owner.address.to_le_bytes());
                payload[22..24].copy_from_slice(&sequence.to_le_bytes());
                payload[24..26].copy_from_slice(&owner.surface_bytes.to_le_bytes());
                payload[26] = hops;
                27
            }
            Content::FindOwners {
                network,
                seeker,
                request,
                remaining,
            } => {
                payload[4] = 6;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                payload[14..22].copy_from_slice(&seeker.to_le_bytes());
                payload[22..24].copy_from_slice(&request.to_le_bytes());
                payload[24] = remaining;
                25
            }
            Content::OwnerFound {
                network,
                seeker,
                request,
                owner,
                hops,
            } => {
                payload[4] = 7;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                payload[14..22].copy_from_slice(&seeker.to_le_bytes());
                payload[22..24].copy_from_slice(&request.to_le_bytes());
                payload[24..32].copy_from_slice(&owner.address.to_le_bytes());
                payload[32..34].copy_from_slice(&owner.surface_bytes.to_le_bytes());
                payload[34] = hops;
                35
            }
            Content::ReadSurface {
                network,
                read,
                remaining,
            } => {
                payload[4] = 4;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                read.encode(&mut payload[14..34]);
                payload[34] = remaining;
                35
            }
            Content::SurfacePart {
                network,
                read,
                total,
                hops,
                bytes,
            } => {
                assert!(read.offset <= total);
                assert_eq!(
                    bytes.len(),
                    SURFACE_BYTES.min(usize::from(total - read.offset))
                );
                payload[4] = 5;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                read.encode(&mut payload[14..34]);
                payload[34..36].copy_from_slice(&total.to_le_bytes());
                payload[36] = hops;
                payload[37..37 + bytes.len()].copy_from_slice(bytes);
                37 + bytes.len()
            }
            Content::Invoke {
                network,
                call,
                remaining,
                arguments,
                copies,
            } => {
                assert!(valid_arguments(arguments));
                assert!((1..=2).contains(&copies));
                assert!(copies == 1 || self.destination.is_some());
                payload[4] = 8;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                call.encode(&mut payload[14..42]);
                payload[42] = remaining | ((copies - 1) << 4);
                payload[43..43 + arguments.len()].copy_from_slice(arguments);
                43 + arguments.len()
            }
            Content::InvocationResult {
                network,
                call,
                status,
                hops,
            } => {
                payload[4] = 9;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                call.encode(&mut payload[14..42]);
                payload[42] = status as u8;
                payload[43] = hops;
                44
            }
            Content::LinkExchange {
                network,
                phase,
                token,
            } => {
                payload[4] = 13;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                payload[14] = phase as u8;
                payload[15..19].copy_from_slice(&token.to_le_bytes());
                19
            }
            Content::ReadLinks {
                network,
                read,
                remaining,
            } => {
                payload[4] = 14;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                read.encode(&mut payload[14..48]);
                payload[48] = remaining;
                49
            }
            Content::LinkEntry {
                network,
                read,
                revision,
                link,
                hops,
            } => {
                payload[4] = 15;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                read.encode(&mut payload[14..48]);
                payload[48] = hops;
                payload[49..53].copy_from_slice(&revision.to_le_bytes());
                if let Some(link) = link {
                    payload[53] = 1;
                    payload[54..62].copy_from_slice(&link.peer.to_le_bytes());
                    payload[62..64].copy_from_slice(&link.round_trip_ms.to_le_bytes());
                    64
                } else {
                    payload[53] = 0;
                    54
                }
            }
            Content::ReadState {
                network,
                read,
                remaining,
            } => {
                payload[4] = 11;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                read.encode(&mut payload[14..40]);
                payload[40] = remaining;
                41
            }
            Content::StateValue {
                network,
                read,
                result,
                hops,
            } => {
                payload[4] = 12;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                read.encode(&mut payload[14..40]);
                payload[40] = hops;
                match result {
                    StateReadResult::Value { sequence, value } => {
                        payload[41] = 0;
                        payload[42..46].copy_from_slice(&sequence.to_le_bytes());
                        if let Some(value) = value {
                            46 + encode_value(value, &mut payload[46..])
                        } else {
                            payload[46] = 255;
                            47
                        }
                    }
                    StateReadResult::UnknownAttribute => {
                        payload[41] = 1;
                        42
                    }
                    StateReadResult::Busy => {
                        payload[41] = 2;
                        42
                    }
                }
            }
            Content::StateChanged {
                network,
                delta,
                hops,
            } => {
                payload[4] = 10;
                payload[5..13].copy_from_slice(&network.founder.to_le_bytes());
                payload[13] = network.channel;
                payload[14..22].copy_from_slice(&delta.owner.to_le_bytes());
                payload[22..30].copy_from_slice(&delta.attribute.to_le_bytes());
                payload[30..34].copy_from_slice(&delta.sequence.to_le_bytes());
                payload[34] = hops;
                if let Some(value) = delta.value {
                    35 + encode_value(value, &mut payload[35..])
                } else {
                    payload[35] = 255;
                    36
                }
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
                ack_request: self.destination.is_some(),
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

    pub fn decode(bytes: &'a [u8], local: u64, channel: u8) -> Result<Self, InvalidFrame> {
        if bytes.len() > MAX_FRAME_SIZE || bytes.len() < 3 {
            return Err(InvalidFrame);
        }
        // Exact profile: 2006 data, compressed PAN, extended source, short
        // broadcast/extended destination, optional unicast ACK, no security/IEs.
        if !matches!((bytes[0], bytes[1]), (0x41, 0xd8 | 0xdc) | (0x61, 0xdc)) {
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
                match p[4] {
                    1 => Content::Present(network),
                    _ => Content::Propose(network),
                }
            }
            (3, 27, None) | (6, 25, None) | (7, 35, Some(_)) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                if !network.valid() || network.channel != channel {
                    return Err(InvalidFrame);
                }
                let address = u64::from_le_bytes(p[14..22].try_into().unwrap());
                let token = u16::from_le_bytes(p[22..24].try_into().unwrap());
                if !valid_address(address) {
                    return Err(InvalidFrame);
                }
                if p[4] == 6 {
                    let remaining = p[24];
                    if !(1..=MAX_HOPS).contains(&remaining)
                        || (remaining == MAX_HOPS && source != address)
                    {
                        return Err(InvalidFrame);
                    }
                    Content::FindOwners {
                        network,
                        seeker: address,
                        request: token,
                        remaining,
                    }
                } else {
                    let (owner, hops) = if p[4] == 3 {
                        (
                            SurfaceOwner {
                                address,
                                surface_bytes: u16::from_le_bytes(p[24..26].try_into().unwrap()),
                            },
                            p[26],
                        )
                    } else {
                        (
                            SurfaceOwner {
                                address: u64::from_le_bytes(p[24..32].try_into().unwrap()),
                                surface_bytes: u16::from_le_bytes(p[32..34].try_into().unwrap()),
                            },
                            p[34],
                        )
                    };
                    if !valid_address(owner.address)
                        || owner.surface_bytes < 13
                        || !(1..=MAX_HOPS).contains(&hops)
                        || (hops == 1 && source != owner.address)
                    {
                        return Err(InvalidFrame);
                    }
                    if p[4] == 3 {
                        Content::Arrive {
                            network,
                            owner,
                            sequence: token,
                            hops,
                        }
                    } else if owner.address != address {
                        Content::OwnerFound {
                            network,
                            seeker: address,
                            request: token,
                            owner,
                            hops,
                        }
                    } else {
                        return Err(InvalidFrame);
                    }
                }
            }
            (4, 35, _) | (5, 37..=MAX_PAYLOAD, Some(_)) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                let read = SurfaceRead::decode(&p[14..34]);
                if !network.valid()
                    || network.channel != channel
                    || !valid_address(read.reader)
                    || !valid_address(read.owner)
                    || read.reader == read.owner
                {
                    return Err(InvalidFrame);
                }
                if p[4] == 4 {
                    let remaining = p[34];
                    if !(1..=MAX_HOPS).contains(&remaining)
                        || (remaining == MAX_HOPS && source != read.reader)
                    {
                        return Err(InvalidFrame);
                    }
                    Content::ReadSurface {
                        network,
                        read,
                        remaining,
                    }
                } else {
                    let total = u16::from_le_bytes(p[34..36].try_into().unwrap());
                    let hops = p[36];
                    if total < 13
                        || read.offset > total
                        || !(1..=MAX_HOPS).contains(&hops)
                        || (hops == 1 && source != read.owner)
                        || p.len() - 37 != SURFACE_BYTES.min(usize::from(total - read.offset))
                    {
                        return Err(InvalidFrame);
                    }
                    Content::SurfacePart {
                        network,
                        read,
                        total,
                        hops,
                        bytes: &p[37..],
                    }
                }
            }
            (8, 44..=MAX_PAYLOAD, _) | (9, 44, Some(_)) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                let call = Invocation::decode(&p[14..42]);
                if !network.valid()
                    || network.channel != channel
                    || !valid_address(call.caller)
                    || !valid_address(call.owner)
                    || call.caller == call.owner
                    || !valid_address(call.transformer)
                {
                    return Err(InvalidFrame);
                }
                if p[4] == 8 {
                    let remaining = p[42] & 0x0f;
                    let copies = 1 + (p[42] >> 4);
                    if copies > 2
                        || (copies > 1 && destination.is_none())
                        || !(1..=MAX_HOPS).contains(&remaining)
                        || (remaining == MAX_HOPS && source != call.caller)
                        || !valid_arguments(&p[43..])
                    {
                        return Err(InvalidFrame);
                    }
                    Content::Invoke {
                        network,
                        call,
                        remaining,
                        copies,
                        arguments: &p[43..],
                    }
                } else {
                    let status = match p[42] {
                        0 => InvokeStatus::Invoked,
                        1 => InvokeStatus::Arguments,
                        2 => InvokeStatus::UnknownTransformer,
                        3 => InvokeStatus::ApplianceError,
                        4 => InvokeStatus::Busy,
                        5 => InvokeStatus::ConflictingRequest,
                        _ => return Err(InvalidFrame),
                    };
                    let hops = p[43];
                    if !(1..=MAX_HOPS).contains(&hops) || (hops == 1 && source != call.owner) {
                        return Err(InvalidFrame);
                    }
                    Content::InvocationResult {
                        network,
                        call,
                        status,
                        hops,
                    }
                }
            }
            (10, 36..=44, None) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                let owner = u64::from_le_bytes(p[14..22].try_into().unwrap());
                let attribute = u64::from_le_bytes(p[22..30].try_into().unwrap());
                let sequence = u32::from_le_bytes(p[30..34].try_into().unwrap());
                let hops = p[34];
                if !network.valid()
                    || network.channel != channel
                    || !valid_address(owner)
                    || !valid_address(attribute)
                    || !(1..=MAX_HOPS).contains(&hops)
                    || (hops == 1 && owner != source)
                {
                    return Err(InvalidFrame);
                }
                let value = if p[35] == 255 {
                    if p.len() != 36 {
                        return Err(InvalidFrame);
                    }
                    None
                } else {
                    let (value, n) = decode_value(&p[35..]).ok_or(InvalidFrame)?;
                    if n != p.len() - 35 {
                        return Err(InvalidFrame);
                    }
                    Some(value)
                };
                Content::StateChanged {
                    network,
                    delta: StateChange {
                        owner,
                        attribute,
                        sequence,
                        value,
                    },
                    hops,
                }
            }
            (11, 41, _) | (12, 42..=55, Some(_)) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                let read = StateRead::decode(&p[14..40]);
                let scope = p[40];
                if !network.valid()
                    || network.channel != channel
                    || !valid_address(read.reader)
                    || !valid_address(read.owner)
                    || !valid_address(read.attribute)
                    || read.reader == read.owner
                    || !(1..=MAX_HOPS).contains(&scope)
                {
                    return Err(InvalidFrame);
                }
                if p[4] == 11 {
                    if scope == MAX_HOPS && source != read.reader {
                        return Err(InvalidFrame);
                    }
                    Content::ReadState {
                        network,
                        read,
                        remaining: scope,
                    }
                } else {
                    if scope == 1 && source != read.owner {
                        return Err(InvalidFrame);
                    }
                    let result = match (p[41], p.len()) {
                        (0, 47..=55) => {
                            let sequence = u32::from_le_bytes(p[42..46].try_into().unwrap());
                            let value = if p[46] == 255 && p.len() == 47 {
                                None
                            } else {
                                let (value, n) = decode_value(&p[46..]).ok_or(InvalidFrame)?;
                                if n != p.len() - 46 {
                                    return Err(InvalidFrame);
                                }
                                Some(value)
                            };
                            StateReadResult::Value { sequence, value }
                        }
                        (1, 42) => StateReadResult::UnknownAttribute,
                        (2, 42) => StateReadResult::Busy,
                        _ => return Err(InvalidFrame),
                    };
                    Content::StateValue {
                        network,
                        read,
                        result,
                        hops: scope,
                    }
                }
            }
            (13, 19, Some(_)) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                if !network.valid() || network.channel != channel {
                    return Err(InvalidFrame);
                }
                let phase = match p[14] {
                    0 => LinkPhase::Offer,
                    1 => LinkPhase::Accept,
                    2 => LinkPhase::Confirm,
                    3 => LinkPhase::Reject,
                    _ => return Err(InvalidFrame),
                };
                let token = u32::from_le_bytes(p[15..19].try_into().unwrap());
                if token == 0 {
                    return Err(InvalidFrame);
                }
                Content::LinkExchange {
                    network,
                    phase,
                    token,
                }
            }
            (14, 49, _) | (15, 54 | 64, Some(_)) => {
                let network = Network {
                    founder: u64::from_le_bytes(p[5..13].try_into().unwrap()),
                    channel: p[13],
                };
                let read = LinkRead::decode(&p[14..48]);
                let scope = p[48];
                if !network.valid()
                    || network.channel != channel
                    || !valid_address(read.reader)
                    || !valid_address(read.owner)
                    || read.reader == read.owner
                    || (read.via != 0
                        && (!valid_address(read.via)
                            || read.via == read.reader
                            || read.via == read.owner))
                    || !(1..=MAX_HOPS).contains(&scope)
                {
                    return Err(InvalidFrame);
                }
                if p[4] == 14 {
                    if scope == MAX_HOPS && source != read.reader {
                        return Err(InvalidFrame);
                    }
                    Content::ReadLinks {
                        network,
                        read,
                        remaining: scope,
                    }
                } else {
                    if scope == 1 && source != read.owner {
                        return Err(InvalidFrame);
                    }
                    let revision = u32::from_le_bytes(p[49..53].try_into().unwrap());
                    let link = match (p[53], p.len()) {
                        (0, 54) => None,
                        (1, 64) => {
                            let peer = u64::from_le_bytes(p[54..62].try_into().unwrap());
                            let round_trip_ms = u16::from_le_bytes(p[62..64].try_into().unwrap());
                            if !valid_address(peer)
                                || peer == read.owner
                                || peer <= read.after
                                || round_trip_ms == 0
                            {
                                return Err(InvalidFrame);
                            }
                            Some(PeerLink {
                                peer,
                                round_trip_ms,
                            })
                        }
                        _ => return Err(InvalidFrame),
                    };
                    Content::LinkEntry {
                        network,
                        read,
                        revision,
                        link,
                        hops: scope,
                    }
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

impl Invocation {
    fn encode(self, out: &mut [u8]) {
        out[..8].copy_from_slice(&self.caller.to_le_bytes());
        out[8..16].copy_from_slice(&self.owner.to_le_bytes());
        out[16..20].copy_from_slice(&self.request.to_le_bytes());
        out[20..28].copy_from_slice(&self.transformer.to_le_bytes());
    }
    fn decode(bytes: &[u8]) -> Self {
        Self {
            caller: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            owner: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            request: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            transformer: u64::from_le_bytes(bytes[20..28].try_into().unwrap()),
        }
    }
}

impl LinkRead {
    fn encode(self, out: &mut [u8]) {
        out[..8].copy_from_slice(&self.reader.to_le_bytes());
        out[8..16].copy_from_slice(&self.owner.to_le_bytes());
        out[16..18].copy_from_slice(&self.request.to_le_bytes());
        out[18..26].copy_from_slice(&self.after.to_le_bytes());
        out[26..34].copy_from_slice(&self.via.to_le_bytes());
    }
    fn decode(bytes: &[u8]) -> Self {
        Self {
            reader: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            owner: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            request: u16::from_le_bytes(bytes[16..18].try_into().unwrap()),
            after: u64::from_le_bytes(bytes[18..26].try_into().unwrap()),
            via: u64::from_le_bytes(bytes[26..34].try_into().unwrap()),
        }
    }
}

impl StateRead {
    fn encode(self, out: &mut [u8]) {
        out[..8].copy_from_slice(&self.reader.to_le_bytes());
        out[8..16].copy_from_slice(&self.owner.to_le_bytes());
        out[16..18].copy_from_slice(&self.request.to_le_bytes());
        out[18..26].copy_from_slice(&self.attribute.to_le_bytes());
    }

    fn decode(bytes: &[u8]) -> Self {
        Self {
            reader: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            owner: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            request: u16::from_le_bytes(bytes[16..18].try_into().unwrap()),
            attribute: u64::from_le_bytes(bytes[18..26].try_into().unwrap()),
        }
    }
}

fn valid_address(address: u64) -> bool {
    address != 0 && address != u64::MAX
}

impl SurfaceRead {
    fn encode(self, out: &mut [u8]) {
        out[..8].copy_from_slice(&self.reader.to_le_bytes());
        out[8..16].copy_from_slice(&self.owner.to_le_bytes());
        out[16..18].copy_from_slice(&self.request.to_le_bytes());
        out[18..20].copy_from_slice(&self.offset.to_le_bytes());
    }

    fn decode(bytes: &[u8]) -> Self {
        Self {
            reader: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            owner: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            request: u16::from_le_bytes(bytes[16..18].try_into().unwrap()),
            offset: u16::from_le_bytes(bytes[18..20].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: u64 = 0x0102_0304_0506_0708;
    const B: u64 = 0x1112_1314_1516_1718;

    #[test]
    fn invocation_copy_budget_is_packed_and_cannot_authorize_broadcast_forks() {
        let network = Network {
            founder: 99,
            channel: 15,
        };
        let call = Invocation {
            caller: A,
            owner: B,
            request: 7,
            transformer: 42,
        };
        let mut wire = [0; MAX_FRAME_SIZE];
        for copies in [1, 2] {
            let message = Message {
                source: A,
                destination: Some(B),
                content: Content::Invoke {
                    network,
                    call,
                    remaining: 8,
                    copies,
                    arguments: &[1, 0, 1],
                },
            };
            let n = message.encode(0, &mut wire);
            assert_eq!(n, 67);
            assert_eq!(wire[21 + 42], 8 | ((copies - 1) << 4));
            assert_eq!(Message::decode(&wire[..n], B, 15).unwrap(), message);
            for invalid in [0, 9, 0x10, 0x19, 0x28, 0x88] {
                let mut bad = wire;
                bad[21 + 42] = invalid;
                assert!(Message::decode(&bad[..n], B, 15).is_err());
            }
            wire[21 + 3] = 5;
            assert!(
                Message::decode(&wire[..n], B, 15).is_err(),
                "v5 lacks the copy budget"
            );
        }
        let message = Message {
            source: A,
            destination: None,
            content: Content::Invoke {
                network,
                call,
                remaining: 8,
                copies: 1,
                arguments: &[0],
            },
        };
        let n = message.encode(0, &mut wire);
        wire[15 + 42] |= 0x10;
        assert!(Message::decode(&wire[..n], B, 15).is_err());
    }

    #[test]
    fn link_exchange_and_cursor_records_fit_individual_frames_and_validate_direction() {
        let network = Network {
            founder: 99,
            channel: 15,
        };
        let read = LinkRead {
            reader: A,
            owner: B,
            request: 0x1234,
            after: 0,
            via: A + 1,
        };
        let mut wire = [0; MAX_FRAME_SIZE];
        for phase in [
            LinkPhase::Offer,
            LinkPhase::Accept,
            LinkPhase::Confirm,
            LinkPhase::Reject,
        ] {
            let message = Message {
                source: A,
                destination: Some(B),
                content: Content::LinkExchange {
                    network,
                    phase,
                    token: 0x04030201,
                },
            };
            let n = message.encode(7, &mut wire);
            assert_eq!(n, 40);
            assert_eq!(
                &wire[21..n],
                &[
                    b'L',
                    b'V',
                    b'T',
                    6,
                    13,
                    99,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    15,
                    phase as u8,
                    1,
                    2,
                    3,
                    4
                ]
            );
            assert_eq!(Message::decode(&wire[..n], B, 15).unwrap(), message);
            for end in 0..n {
                assert!(Message::decode(&wire[..end], B, 15).is_err());
            }
            assert!(Message::decode(&wire[..n + 1], B, 15).is_err());
            let mut bad = wire;
            bad[21 + 14] = 4;
            assert!(Message::decode(&bad[..n], B, 15).is_err());
            bad = wire;
            bad[21 + 15..n].fill(0);
            assert!(Message::decode(&bad[..n], B, 15).is_err());
            let n = Message {
                destination: None,
                ..message
            }
            .encode(7, &mut wire);
            assert!(Message::decode(&wire[..n], B, 15).is_err());
        }
        let query = Message {
            source: A,
            destination: Some(B),
            content: Content::ReadLinks {
                network,
                read,
                remaining: 8,
            },
        };
        let n = query.encode(1, &mut wire);
        assert_eq!(n, 70);
        assert_eq!(
            &wire[21..n],
            &[
                b'L', b'V', b'T', 6, 14, 99, 0, 0, 0, 0, 0, 0, 0, 15, 8, 7, 6, 5, 4, 3, 2, 1, 24,
                23, 22, 21, 20, 19, 18, 17, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 9, 7, 6, 5, 4, 3,
                2, 1, 8,
            ]
        );
        for link in [
            None,
            Some(PeerLink {
                peer: A,
                round_trip_ms: 3,
            }),
        ] {
            let reply = Message {
                source: B,
                destination: Some(A),
                content: Content::LinkEntry {
                    network,
                    read,
                    revision: 0x04030201,
                    link,
                    hops: 1,
                },
            };
            for message in [query, reply] {
                let local = message.destination.unwrap();
                let n = message.encode(1, &mut wire);
                assert_eq!(
                    n,
                    if local == B {
                        70
                    } else if link.is_some() {
                        85
                    } else {
                        75
                    }
                );
                assert_eq!(Message::decode(&wire[..n], local, 15).unwrap(), message);
                for end in 0..n {
                    assert!(Message::decode(&wire[..end], local, 15).is_err());
                }
                assert!(Message::decode(&wire[..n + 1], local, 15).is_err());
                assert!(Message::decode(&wire[..n], local, 16).is_err());
                for hop in [0, 9] {
                    let mut bad = wire;
                    bad[21 + 48] = hop;
                    assert!(Message::decode(&bad[..n], local, 15).is_err());
                }
                for via in [A, B, u64::MAX] {
                    let mut bad = wire;
                    bad[21 + 40..21 + 48].copy_from_slice(&via.to_le_bytes());
                    assert!(Message::decode(&bad[..n], local, 15).is_err());
                }
            }
            let n = reply.encode(2, &mut wire);
            assert_eq!(&wire[21 + 49..21 + 53], &0x04030201u32.to_le_bytes());
            let mut bad = wire;
            bad[21 + 53] = 2;
            assert!(Message::decode(&bad[..n], A, 15).is_err());
            if link.is_some() {
                for peer in [0, B, u64::MAX] {
                    let mut bad = wire;
                    bad[21 + 54..21 + 62].copy_from_slice(&peer.to_le_bytes());
                    assert!(Message::decode(&bad[..n], A, 15).is_err());
                }
                let mut bad = wire;
                bad[21 + 62..21 + 64].fill(0);
                assert!(Message::decode(&bad[..n], A, 15).is_err());
                bad = wire;
                bad[21 + 32..21 + 40].copy_from_slice(&A.to_le_bytes());
                assert!(
                    Message::decode(&bad[..n], A, 15).is_err(),
                    "cursor repeated a prior entry"
                );
            }
            let n = Message {
                destination: None,
                ..reply
            }
            .encode(3, &mut wire);
            assert!(Message::decode(&wire[..n], A, 15).is_err());
            let n = Message {
                source: A + 1,
                ..reply
            }
            .encode(4, &mut wire);
            assert!(
                Message::decode(&wire[..n], A, 15).is_err(),
                "non-owner claimed first hop"
            );
            let forwarded = Message {
                source: A + 1,
                content: Content::LinkEntry {
                    network,
                    read,
                    revision: 1,
                    link,
                    hops: 2,
                },
                ..reply
            };
            let n = forwarded.encode(5, &mut wire);
            assert_eq!(Message::decode(&wire[..n], A, 15).unwrap(), forwarded);
        }
        let n = Message {
            destination: None,
            content: Content::ReadLinks {
                network,
                read: LinkRead {
                    via: 0,
                    after: u64::MAX,
                    ..read
                },
                remaining: 8,
            },
            ..query
        }
        .encode(6, &mut wire);
        assert_eq!(n, 64);
        assert!(Message::decode(&wire[..n], B, 15).is_ok());
    }

    #[test]
    fn state_read_frames_are_complete_and_distinguish_values_from_failures() {
        let network = Network {
            founder: 99,
            channel: 15,
        };
        let read = StateRead {
            reader: A,
            owner: B,
            request: 0x1234,
            attribute: 42,
        };
        let request = Message {
            source: A,
            destination: Some(B),
            content: Content::ReadState {
                network,
                read,
                remaining: 8,
            },
        };
        let mut wire = [0; MAX_FRAME_SIZE];
        let n = request.encode(7, &mut wire);
        assert_eq!(n, 62);
        assert_eq!(
            &wire[21..n],
            &[
                b'L', b'V', b'T', 6, 11, 99, 0, 0, 0, 0, 0, 0, 0, 15, 8, 7, 6, 5, 4, 3, 2, 1, 24,
                23, 22, 21, 20, 19, 18, 17, 0x34, 0x12, 42, 0, 0, 0, 0, 0, 0, 0, 8,
            ]
        );
        for (result, length, tag) in [
            (StateReadResult::UnknownAttribute, 63, 1),
            (StateReadResult::Busy, 63, 2),
            (
                StateReadResult::Value {
                    sequence: 0,
                    value: None,
                },
                68,
                0,
            ),
            (
                StateReadResult::Value {
                    sequence: 0x04030201,
                    value: Some(Value::boolean(false)),
                },
                69,
                0,
            ),
            (
                StateReadResult::Value {
                    sequence: u32::MAX,
                    value: Some(Value::integer(Integer::I64, i64::MIN as i128).unwrap()),
                },
                76,
                0,
            ),
        ] {
            let reply = Message {
                source: B,
                destination: Some(A),
                content: Content::StateValue {
                    network,
                    read,
                    result,
                    hops: 1,
                },
            };
            let n = reply.encode(8, &mut wire);
            assert_eq!(n, length);
            assert_eq!(wire[21 + 41], tag);
            if let StateReadResult::Value { sequence, .. } = result {
                assert_eq!(&wire[21 + 42..21 + 46], &sequence.to_le_bytes());
            }
            for message in [request, reply] {
                let local = message.destination.unwrap();
                let n = message.encode(1, &mut wire);
                assert_eq!(Message::decode(&wire[..n], local, 15).unwrap(), message);
                for end in 0..n {
                    assert!(Message::decode(&wire[..end], local, 15).is_err());
                }
                assert!(Message::decode(&wire[..n + 1], local, 15).is_err());
                assert!(Message::decode(&wire[..n], local, 16).is_err());
                for range in [14..22, 22..30, 32..40] {
                    for byte in [0, 255] {
                        let mut bad = wire;
                        bad[21 + range.start..21 + range.end].fill(byte);
                        assert!(Message::decode(&bad[..n], local, 15).is_err());
                    }
                }
                for hop in [0, 9] {
                    let mut bad = wire;
                    bad[21 + 40] = hop;
                    assert!(Message::decode(&bad[..n], local, 15).is_err());
                }
                let forwarded = Message {
                    source: B + 1,
                    content: match message.content {
                        Content::ReadState { network, read, .. } => Content::ReadState {
                            network,
                            read,
                            remaining: 7,
                        },
                        Content::StateValue {
                            network,
                            read,
                            result,
                            ..
                        } => Content::StateValue {
                            network,
                            read,
                            result,
                            hops: 2,
                        },
                        _ => unreachable!(),
                    },
                    ..message
                };
                let n = forwarded.encode(2, &mut wire);
                assert_eq!(Message::decode(&wire[..n], local, 15).unwrap(), forwarded);
                wire[21 + 40] = if local == B { 8 } else { 1 };
                assert!(
                    Message::decode(&wire[..n], local, 15).is_err(),
                    "forged first hop accepted"
                );
            }
            let n = reply.encode(3, &mut wire);
            wire[21 + 41] = 3;
            assert!(Message::decode(&wire[..n], A, 15).is_err());
            let n = Message {
                destination: None,
                ..reply
            }
            .encode(4, &mut wire);
            assert!(
                Message::decode(&wire[..n], A, 15).is_err(),
                "state read result broadcast"
            );
        }
        let n = Message {
            destination: None,
            ..request
        }
        .encode(5, &mut wire);
        assert_eq!(n, 56);
        assert!(Message::decode(&wire[..n], B, 15).is_ok());
        let bad_value = Message {
            source: B,
            destination: Some(A),
            content: Content::StateValue {
                network,
                read,
                result: StateReadResult::Value {
                    sequence: 1,
                    value: Some(Value::boolean(true)),
                },
                hops: 1,
            },
        };
        let n = bad_value.encode(6, &mut wire);
        wire[21 + 47] = 2;
        assert!(Message::decode(&wire[..n], A, 15).is_err());
        wire[21 + 46] = 255;
        assert!(
            Message::decode(&wire[..n], A, 15).is_err(),
            "unavailable value had trailing bytes"
        );
    }

    #[test]
    fn scalar_arguments_keep_width_signedness_and_fit_one_frame() {
        let mut bytes = [0; CALL_ARGUMENT_BYTES];
        let mut decoded = [Value::boolean(false); MAX_CALL_ARGUMENTS];
        for (ty, lo, hi, tag, width) in [
            (Integer::U8, 0, u8::MAX as i128, 1, 1),
            (Integer::U16, 0, u16::MAX as i128, 2, 2),
            (Integer::U32, 0, u32::MAX as i128, 3, 4),
            (Integer::U64, 0, u64::MAX as i128, 4, 8),
            (Integer::I8, i8::MIN as i128, i8::MAX as i128, 5, 1),
            (Integer::I16, i16::MIN as i128, i16::MAX as i128, 6, 2),
            (Integer::I32, i32::MIN as i128, i32::MAX as i128, 7, 4),
            (Integer::I64, i64::MIN as i128, i64::MAX as i128, 8, 8),
        ] {
            for value in [lo, hi] {
                let values = [Value::integer(ty, value).unwrap()];
                let n = encode_arguments(&values, &mut bytes).unwrap();
                assert_eq!(n, 2 + width);
                assert_eq!(&bytes[..2], &[1, tag]);
                assert_eq!(&bytes[2..n], &(value as u64).to_le_bytes()[..width]);
                assert_eq!(decode_arguments(&bytes[..n], &mut decoded).unwrap(), values);
                for end in 0..n {
                    assert!(decode_arguments(&bytes[..end], &mut decoded).is_none());
                }
            }
        }
        for value in [false, true] {
            assert_eq!(
                encode_arguments(&[Value::boolean(value)], &mut bytes),
                Some(3)
            );
            assert_eq!(&bytes[..3], &[1, 0, u8::from(value)]);
        }
        assert_eq!(encode_arguments(&[], &mut bytes), Some(1));
        assert_eq!(decode_arguments(&bytes[..1], &mut decoded).unwrap(), &[]);
        for bad in [
            &[][..],
            &[1],
            &[1, 0, 2],
            &[0, 0],
            &[1, 255],
            &[1, 9, 0],
            &[2, 0, 1],
        ] {
            assert!(decode_arguments(bad, &mut decoded).is_none());
        }
        assert_eq!(
            encode_arguments(&[Value::boolean(true); 30], &mut bytes),
            Some(61)
        );
        let call = Invocation {
            caller: A,
            owner: B,
            request: 7,
            transformer: 42,
        };
        let message = Message {
            source: A,
            destination: Some(B),
            content: Content::Invoke {
                copies: 1,
                network: Network {
                    founder: 99,
                    channel: 15,
                },
                call,
                remaining: MAX_HOPS,
                arguments: &bytes,
            },
        };
        let mut wire = [0; MAX_FRAME_SIZE];
        let n = message.encode(0, &mut wire);
        assert_eq!(n, MAX_FRAME_SIZE);
        assert_eq!(Message::decode(&wire[..n], B, 15).unwrap(), message);
        assert_eq!(
            encode_arguments(&[Value::boolean(true); 31], &mut bytes),
            None
        );
        let large = Value::integer(Integer::U64, u64::MAX as i128).unwrap();
        assert_eq!(encode_arguments(&[large; 6], &mut bytes), Some(55));
        assert_eq!(encode_arguments(&[large; 7], &mut bytes), None);
    }

    #[test]
    fn control_and_delta_envelopes_are_complete_numeric_messages() {
        let network = Network {
            founder: 99,
            channel: 15,
        };
        let call = Invocation {
            caller: A,
            owner: B,
            request: 0x04030201,
            transformer: 0x0807060504030201,
        };
        let request = Message {
            source: A,
            destination: Some(B),
            content: Content::Invoke {
                copies: 1,
                network,
                call,
                remaining: MAX_HOPS,
                arguments: &[1, 0, 1],
            },
        };
        let mut wire = [0; MAX_FRAME_SIZE];
        let n = request.encode(7, &mut wire);
        assert_eq!(n, 67);
        assert_eq!(
            &wire[21..n],
            &[
                b'L', b'V', b'T', 6, 8, 99, 0, 0, 0, 0, 0, 0, 0, 15, 8, 7, 6, 5, 4, 3, 2, 1, 24,
                23, 22, 21, 20, 19, 18, 17, 1, 2, 3, 4, 1, 2, 3, 4, 5, 6, 7, 8, 8, 1, 0, 1,
            ]
        );
        for message in [
            request,
            Message {
                source: B,
                destination: Some(A),
                content: Content::InvocationResult {
                    network,
                    call,
                    status: InvokeStatus::Invoked,
                    hops: 1,
                },
            },
            Message {
                source: A,
                destination: None,
                content: Content::StateChanged {
                    network,
                    delta: StateChange {
                        owner: A,
                        attribute: 42,
                        sequence: 17,
                        value: None,
                    },
                    hops: 1,
                },
            },
            Message {
                source: A,
                destination: None,
                content: Content::StateChanged {
                    network,
                    delta: StateChange {
                        owner: A,
                        attribute: 42,
                        sequence: 18,
                        value: Some(Value::boolean(false)),
                    },
                    hops: 1,
                },
            },
            Message {
                source: A,
                destination: None,
                content: Content::StateChanged {
                    network,
                    delta: StateChange {
                        owner: A,
                        attribute: 42,
                        sequence: 19,
                        value: Some(Value::integer(Integer::I64, -1).unwrap()),
                    },
                    hops: 1,
                },
            },
        ] {
            let local = if message.source == A { B } else { A };
            let mut wire = [0; MAX_FRAME_SIZE];
            let n = message.encode(7, &mut wire);
            assert_eq!(Message::decode(&wire[..n], local, 15).unwrap(), message);
            assert!(Message::decode(&wire[..n], local, 16).is_err());
            for end in 0..n {
                assert!(Message::decode(&wire[..end], local, 15).is_err());
            }
            assert!(Message::decode(&wire[..n + 1], local, 15).is_err());
            let header = if message.destination.is_some() {
                21
            } else {
                15
            };
            let hop = match message.content {
                Content::Invoke { .. } => 42,
                Content::InvocationResult { .. } => 43,
                _ => 34,
            };
            for value in [0, 9] {
                let mut bad = wire;
                bad[header + hop] = value;
                assert!(Message::decode(&bad[..n], local, 15).is_err());
            }
            for range in [14..22, 22..30] {
                for value in [0, 255] {
                    let mut bad = wire;
                    bad[header + range.start..header + range.end].fill(value);
                    assert!(Message::decode(&bad[..n], local, 15).is_err());
                }
            }
        }
        // Status and scalar tags are strict; neither failure is an application call.
        let reply = Message {
            source: B,
            destination: Some(A),
            content: Content::InvocationResult {
                network,
                call,
                status: InvokeStatus::Busy,
                hops: 1,
            },
        };
        let n = reply.encode(0, &mut wire);
        wire[21 + 42] = 6;
        assert!(Message::decode(&wire[..n], A, 15).is_err());
        let n = request.encode(0, &mut wire);
        wire[21 + 45] = 2;
        assert!(Message::decode(&wire[..n], B, 15).is_err());
    }

    #[test]
    fn owner_discovery_wire_vectors_preserve_origin_and_enforce_scope() {
        let network = Network {
            founder: 99,
            channel: 23,
        };
        let owner = SurfaceOwner {
            address: A,
            surface_bytes: 195,
        };
        let cases: &[(Content, Option<u64>, &[u8])] = &[
            (
                Content::Arrive {
                    network,
                    owner,
                    sequence: 0x1234,
                    hops: 1,
                },
                None,
                &[
                    b'L', b'V', b'T', 6, 3, 99, 0, 0, 0, 0, 0, 0, 0, 23, 8, 7, 6, 5, 4, 3, 2, 1,
                    0x34, 0x12, 195, 0, 1,
                ],
            ),
            (
                Content::FindOwners {
                    network,
                    seeker: A,
                    request: 0x1234,
                    remaining: MAX_HOPS,
                },
                None,
                &[
                    b'L', b'V', b'T', 6, 6, 99, 0, 0, 0, 0, 0, 0, 0, 23, 8, 7, 6, 5, 4, 3, 2, 1,
                    0x34, 0x12, 8,
                ],
            ),
            (
                Content::OwnerFound {
                    network,
                    seeker: B,
                    request: 0x1234,
                    owner,
                    hops: 1,
                },
                Some(B),
                &[
                    b'L', b'V', b'T', 6, 7, 99, 0, 0, 0, 0, 0, 0, 0, 23, 0x18, 0x17, 0x16, 0x15,
                    0x14, 0x13, 0x12, 0x11, 0x34, 0x12, 8, 7, 6, 5, 4, 3, 2, 1, 195, 0, 1,
                ],
            ),
        ];
        for &(content, destination, expected) in cases {
            let message = Message {
                source: A,
                destination,
                content,
            };
            let mut wire = [0; MAX_FRAME_SIZE];
            let n = message.encode(42, &mut wire);
            let header = if destination.is_some() { 21 } else { 15 };
            assert_eq!(&wire[header..n], expected);
            assert_eq!(Message::decode(&wire[..n], B, 23).unwrap(), message);
            assert!(Message::decode(&wire[..n], B, 22).is_err());
            for end in 0..n {
                assert!(Message::decode(&wire[..end], B, 23).is_err());
            }
            assert!(Message::decode(&wire[..n + 1], B, 23).is_err());
            for (field, value) in [
                (3, 4),
                (5, 0),
                (13, 10),
                (expected.len() - 1, 0),
                (expected.len() - 1, MAX_HOPS + 1),
            ] {
                let mut bad = wire;
                bad[header + field] = value;
                assert!(Message::decode(&bad[..n], B, 23).is_err());
            }
            for address in [0u8, 255] {
                let mut bad = wire;
                bad[header + 14..header + 22].fill(address);
                assert!(Message::decode(&bad[..n], B, 23).is_err());
            }
            if !matches!(content, Content::FindOwners { .. }) {
                let owner = if destination.is_some() { 24 } else { 14 };
                let length = if destination.is_some() { 32 } else { 24 };
                let mut bad = wire;
                bad[header + length..header + length + 2].copy_from_slice(&12u16.to_le_bytes());
                assert!(Message::decode(&bad[..n], B, 23).is_err());
                for address in [0, u64::MAX, A + 1, B] {
                    let mut bad = wire;
                    bad[header + owner..header + owner + 8].copy_from_slice(&address.to_le_bytes());
                    assert!(Message::decode(&bad[..n], B, 23).is_err());
                }
            }
            let wrong = Message {
                destination: if destination.is_some() { None } else { Some(B) },
                ..message
            };
            let n = wrong.encode(1, &mut wire);
            assert!(Message::decode(&wire[..n], B, 23).is_err());
            // A forwarded packet has a new link sender but retains its owner/seeker.
            let forwarded = Message {
                source: A + 1,
                destination,
                content: match content {
                    Content::Arrive {
                        network,
                        owner,
                        sequence,
                        ..
                    } => Content::Arrive {
                        network,
                        owner,
                        sequence,
                        hops: 2,
                    },
                    Content::FindOwners {
                        network,
                        seeker,
                        request,
                        ..
                    } => Content::FindOwners {
                        network,
                        seeker,
                        request,
                        remaining: MAX_HOPS - 1,
                    },
                    Content::OwnerFound {
                        network,
                        seeker,
                        request,
                        owner,
                        ..
                    } => Content::OwnerFound {
                        network,
                        seeker,
                        request,
                        owner,
                        hops: 2,
                    },
                    _ => unreachable!(),
                },
            };
            let n = forwarded.encode(1, &mut wire);
            assert_eq!(Message::decode(&wire[..n], B, 23).unwrap(), forwarded);
        }
    }

    #[test]
    fn reads_and_replies_have_bounded_complete_frames() {
        let network = Network {
            founder: 99,
            channel: 23,
        };
        let bytes = [0x5a; SURFACE_BYTES];
        for offset in [0, 13, 67, 99, 100] {
            let read = SurfaceRead {
                reader: B,
                owner: A,
                request: 7,
                offset,
            };
            let message = Message {
                source: A,
                destination: Some(B),
                content: Content::SurfacePart {
                    network,
                    read,
                    total: 100,
                    hops: 1,
                    bytes: &bytes[..SURFACE_BYTES.min(100 - offset as usize)],
                },
            };
            let mut out = [0; MAX_FRAME_SIZE];
            let n = message.encode(1, &mut out);
            assert_eq!(Message::decode(&out[..n], B, 23).unwrap(), message);
            assert!(Message::decode(&out[..n], B, 22).is_err());
            for end in 0..n {
                assert!(Message::decode(&out[..end], B, 23).is_err());
            }
            // Network, owner, count, and hop fields are checked independently.
            for (field, value) in [(34, 10), (57, 0), (57, MAX_HOPS + 1), (55, 0)] {
                let mut bad = out;
                bad[field] = value;
                assert!(Message::decode(&bad[..n], B, 23).is_err(), "field {field}");
            }
            let broadcast = Message {
                destination: None,
                ..message
            };
            let n = broadcast.encode(1, &mut out);
            assert!(Message::decode(&out[..n], B, 23).is_err());
        }
        for destination in [None, Some(A)] {
            let message = Message {
                source: B,
                destination,
                content: Content::ReadSurface {
                    network,
                    read: SurfaceRead {
                        reader: B,
                        owner: A,
                        request: 0x1234,
                        offset: 0x5678,
                    },
                    remaining: MAX_HOPS,
                },
            };
            let mut out = [0; MAX_FRAME_SIZE];
            let n = message.encode(9, &mut out);
            assert_eq!(Message::decode(&out[..n], A, 23).unwrap(), message);
            let p = if destination.is_some() { 21 } else { 15 };
            assert_eq!(
                &out[p..n],
                &[
                    b'L', b'V', b'T', 6, 4, 99, 0, 0, 0, 0, 0, 0, 0, 23, 0x18, 0x17, 0x16, 0x15,
                    0x14, 0x13, 0x12, 0x11, 8, 7, 6, 5, 4, 3, 2, 1, 0x34, 0x12, 0x78, 0x56, 8
                ]
            );
            for end in 0..n {
                assert!(Message::decode(&out[..end], A, 23).is_err());
            }
            for remaining in [0, MAX_HOPS + 1] {
                out[n - 1] = remaining;
                assert!(Message::decode(&out[..n], A, 23).is_err());
            }
        }
    }

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
                    b'T', 6, 0,
                ],
            ),
            (
                Message {
                    source: A,
                    destination: Some(B),
                    content: Content::Present(network),
                },
                &[
                    0x61, 0xdc, 42, 0x56, 0x4c, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 8,
                    7, 6, 5, 4, 3, 2, 1, b'L', b'V', b'T', 6, 1, 0x28, 0x27, 0x26, 0x25, 0x24,
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
                    b'T', 6, 2, 0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21, 23,
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
                (0, 0xc1),
                (1, 0xe8),
                (1, 0xd9),
                (payload, b'X'),
                (payload + 2, b'D'),
                (payload + 3, 1),
                (payload + 4, 255),
            ] {
                let mut bad = out;
                bad[offset] = value;
                assert!(
                    Message::decode(&bad[..n], B, 23).is_err(),
                    "mutation {offset}:{value}"
                );
            }
            let mut ack = out;
            ack[0] = 0x61;
            assert_eq!(
                Message::decode(&ack[..n], B, 23).is_ok(),
                message.destination.is_some()
            );
            if message.destination.is_some() {
                ack[0] = 0x41;
                assert_eq!(&Message::decode(&ack[..n], B, 23).unwrap(), message);
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
