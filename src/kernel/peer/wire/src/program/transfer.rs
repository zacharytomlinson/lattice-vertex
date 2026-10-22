//! Installation/read messages. Installed means locally validated and wired;
//! it does not mean the distributed program has been enabled.

pub const DATA_BYTES: usize = 48;
pub const REQUEST_BYTES: usize = 56;
pub const REPLY_BYTES: usize = 60;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    Receiving = 1,
    Persisting = 2,
    Installed = 3,
    Busy = 4,
    Unsupported = 5,
    Invalid = 6,
    Bounds = 7,
    Binding = 8,
    Compatibility = 9,
    Storage = 10,
    Conflict = 11,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    Info,
    Begin {
        transfer: u32,
        length: u16,
    },
    Data {
        transfer: u32,
        offset: u16,
        bytes: &'a [u8],
    },
    Commit {
        transfer: u32,
        digest: [u8; 32],
    },
    Abort {
        transfer: u32,
    },
    Read {
        offset: u16,
    },
    Dependencies {
        state: u64,
        after: u16,
    },
}
impl Request<'_> {
    pub fn encode(self, out: &mut [u8; REQUEST_BYTES]) -> usize {
        match self {
            Self::Dependencies { state, after } => {
                out[0] = 6;
                out[1..9].copy_from_slice(&state.to_le_bytes());
                out[9..11].copy_from_slice(&after.to_le_bytes());
                11
            }
            Self::Info => {
                out[0] = 0;
                1
            }
            Self::Begin { transfer, length } => {
                out[0] = 1;
                out[1..5].copy_from_slice(&transfer.to_le_bytes());
                out[5..7].copy_from_slice(&length.to_le_bytes());
                7
            }
            Self::Data {
                transfer,
                offset,
                bytes,
            } => {
                assert!(!bytes.is_empty() && bytes.len() <= DATA_BYTES);
                out[0] = 2;
                out[1..5].copy_from_slice(&transfer.to_le_bytes());
                out[5..7].copy_from_slice(&offset.to_le_bytes());
                out[7] = bytes.len() as u8;
                out[8..8 + bytes.len()].copy_from_slice(bytes);
                8 + bytes.len()
            }
            Self::Commit { transfer, digest } => {
                out[0] = 3;
                out[1..5].copy_from_slice(&transfer.to_le_bytes());
                out[5..37].copy_from_slice(&digest);
                37
            }
            Self::Abort { transfer } => {
                out[0] = 4;
                out[1..5].copy_from_slice(&transfer.to_le_bytes());
                5
            }
            Self::Read { offset } => {
                out[0] = 5;
                out[1..3].copy_from_slice(&offset.to_le_bytes());
                3
            }
        }
    }
}
impl<'a> Request<'a> {
    pub fn decode(b: &'a [u8]) -> Option<Self> {
        Some(match (b.first().copied()?, b.len()) {
            (0, 1) => Self::Info,
            (1, 7) => Self::Begin {
                transfer: u32::from_le_bytes(b[1..5].try_into().ok()?),
                length: u16::from_le_bytes(b[5..7].try_into().ok()?),
            },
            (2, 9..=REQUEST_BYTES) if b[7] as usize == b.len() - 8 => Self::Data {
                transfer: u32::from_le_bytes(b[1..5].try_into().ok()?),
                offset: u16::from_le_bytes(b[5..7].try_into().ok()?),
                bytes: &b[8..],
            },
            (3, 37) => Self::Commit {
                transfer: u32::from_le_bytes(b[1..5].try_into().ok()?),
                digest: b[5..37].try_into().ok()?,
            },
            (4, 5) => Self::Abort {
                transfer: u32::from_le_bytes(b[1..5].try_into().ok()?),
            },
            (5, 3) => Self::Read {
                offset: u16::from_le_bytes(b[1..3].try_into().ok()?),
            },
            (6, 11) => Self::Dependencies {
                state: u64::from_le_bytes(b[1..9].try_into().ok()?),
                after: u16::from_le_bytes(b[9..11].try_into().ok()?),
            },
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply<'a> {
    pub status: Status,
    pub offset: u16,
    pub generation: u32,
    pub accepted: u16,
    pub total: u16,
    pub bytes: &'a [u8],
}
impl Reply<'_> {
    pub fn status(status: Status) -> Self {
        Self {
            status,
            offset: u16::MAX,
            generation: 0,
            accepted: 0,
            total: 0,
            bytes: &[],
        }
    }
    pub fn encode(self, out: &mut [u8; REPLY_BYTES]) -> usize {
        assert!(self.bytes.len() <= DATA_BYTES);
        out[0] = self.status as u8;
        out[1..3].copy_from_slice(&self.offset.to_le_bytes());
        out[3..7].copy_from_slice(&self.generation.to_le_bytes());
        out[7..9].copy_from_slice(&self.accepted.to_le_bytes());
        out[9..11].copy_from_slice(&self.total.to_le_bytes());
        out[11] = self.bytes.len() as u8;
        out[12..12 + self.bytes.len()].copy_from_slice(self.bytes);
        12 + self.bytes.len()
    }
}
impl<'a> Reply<'a> {
    pub fn decode(b: &'a [u8]) -> Option<Self> {
        if !(12..=REPLY_BYTES).contains(&b.len()) || b[11] as usize != b.len() - 12 {
            return None;
        }
        let status = match b[0] {
            0 => Status::Ok,
            1 => Status::Receiving,
            2 => Status::Persisting,
            3 => Status::Installed,
            4 => Status::Busy,
            5 => Status::Unsupported,
            6 => Status::Invalid,
            7 => Status::Bounds,
            8 => Status::Binding,
            9 => Status::Compatibility,
            10 => Status::Storage,
            11 => Status::Conflict,
            _ => return None,
        };
        Some(Self {
            status,
            offset: u16::from_le_bytes(b[1..3].try_into().ok()?),
            generation: u32::from_le_bytes(b[3..7].try_into().ok()?),
            accepted: u16::from_le_bytes(b[7..9].try_into().ok()?),
            total: u16::from_le_bytes(b[9..11].try_into().ok()?),
            bytes: &b[12..],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_bounds() {
        for (request, length) in [
            (Request::Info, 1),
            (
                Request::Begin {
                    transfer: 1,
                    length: 114,
                },
                7,
            ),
            (
                Request::Data {
                    transfer: 1,
                    offset: 0,
                    bytes: &[1],
                },
                9,
            ),
            (
                Request::Data {
                    transfer: 1,
                    offset: u16::MAX,
                    bytes: &[2; DATA_BYTES],
                },
                REQUEST_BYTES,
            ),
            (
                Request::Commit {
                    transfer: 1,
                    digest: [3; 32],
                },
                37,
            ),
            (Request::Abort { transfer: 1 }, 5),
            (Request::Read { offset: 1 }, 3),
            (
                Request::Dependencies {
                    state: 1,
                    after: u16::MAX,
                },
                11,
            ),
        ] {
            let mut bytes = [0; REQUEST_BYTES];
            let n = request.encode(&mut bytes);
            assert_eq!(n, length);
            assert_eq!(Request::decode(&bytes[..n]), Some(request));
            for end in 0..n {
                assert!(Request::decode(&bytes[..end]).is_none());
            }
            if n < REQUEST_BYTES {
                assert!(Request::decode(&bytes[..n + 1]).is_none());
            }
        }
        for data in [&[][..], &[8][..], &[9; DATA_BYTES][..]] {
            let reply = Reply {
                bytes: data,
                ..Reply::status(Status::Installed)
            };
            let mut bytes = [0; REPLY_BYTES];
            let n = reply.encode(&mut bytes);
            assert_eq!(Reply::decode(&bytes[..n]), Some(reply));
            for end in 0..n {
                assert!(Reply::decode(&bytes[..end]).is_none());
            }
            bytes[0] = 12;
            assert!(Reply::decode(&bytes[..n]).is_none());
        }
    }
}
