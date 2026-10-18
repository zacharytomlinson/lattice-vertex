//! Fixed numeric update records shared by radio peers and attached hosts.
//! Transfer acknowledgements describe receiver progress, independently of MAC ACK.

pub const DATA_BYTES: usize = 48;
pub const REQUEST_BYTES: usize = 71;
pub const PROGRESS_BYTES: usize = 28;
pub type PackageId = [u8; 16];

pub const NOTICE_MS: u32 = 2_000;
pub const MAX_ABSENCE_MS: u32 = 600_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AbsencePhase {
    Proposed = 0,
    Leaving = 1,
    Cancel = 2,
}
/// One-hop update absence, measured from receipt; no synchronized network clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Absence {
    pub attempt: u32,
    pub phase: AbsencePhase,
    pub duration_ms: u32,
}
impl Absence {
    pub fn encode(self, out: &mut [u8; 9]) {
        out[..4].copy_from_slice(&self.attempt.to_le_bytes());
        out[4] = self.phase as u8;
        out[5..].copy_from_slice(&self.duration_ms.to_le_bytes());
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 9 {
            return None;
        }
        let attempt = u32::from_le_bytes(bytes[..4].try_into().ok()?);
        let phase = match bytes[4] {
            0 => AbsencePhase::Proposed,
            1 => AbsencePhase::Leaving,
            2 => AbsencePhase::Cancel,
            _ => return None,
        };
        let duration_ms = u32::from_le_bytes(bytes[5..].try_into().ok()?);
        if attempt == 0
            || (phase == AbsencePhase::Cancel && duration_ms != 0)
            || (phase != AbsencePhase::Cancel && !(1..=MAX_ABSENCE_MS).contains(&duration_ms))
        {
            return None;
        }
        Some(Self {
            attempt,
            phase,
            duration_ms,
        })
    }
}

/// Attached-host maintenance only. These operations have no radio encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Control {
    Status = 0,
    SelectTrial = 1,
    Confirm = 2,
    Reset = 3,
}
impl Control {
    pub fn decode(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Status,
            1 => Self::SelectTrial,
            2 => Self::Confirm,
            3 => Self::Reset,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    Header {
        package: PackageId,
        offset: u8,
        data: &'a [u8],
    },
    Prepare {
        package: PackageId,
    },
    Data {
        package: PackageId,
        offset: u32,
        data: &'a [u8],
    },
    Finish {
        package: PackageId,
    },
    Status {
        package: PackageId,
    },
}
impl Request<'_> {
    pub fn package(self) -> PackageId {
        match self {
            Self::Header { package, .. }
            | Self::Prepare { package }
            | Self::Data { package, .. }
            | Self::Finish { package }
            | Self::Status { package } => package,
        }
    }
    pub fn encode(self, out: &mut [u8; REQUEST_BYTES]) -> usize {
        out[1..17].copy_from_slice(&self.package());
        match self {
            Self::Header { offset, data, .. } => {
                assert!(!data.is_empty() && data.len() <= DATA_BYTES);
                out[0] = 0;
                out[17] = offset;
                out[18] = data.len() as u8;
                out[19..19 + data.len()].copy_from_slice(data);
                19 + data.len()
            }
            Self::Prepare { .. } => {
                out[0] = 1;
                17
            }
            Self::Data { offset, data, .. } => {
                assert!(!data.is_empty() && data.len() <= DATA_BYTES);
                out[0] = 2;
                out[17..21].copy_from_slice(&offset.to_le_bytes());
                out[21] = data.len() as u8;
                out[22..22 + data.len()].copy_from_slice(data);
                22 + data.len()
            }
            Self::Finish { .. } => {
                out[0] = 3;
                17
            }
            Self::Status { .. } => {
                out[0] = 4;
                17
            }
        }
    }
}
impl<'a> Request<'a> {
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let package = bytes.get(1..17)?.try_into().ok()?;
        Some(match bytes[0] {
            0 if bytes.len() >= 20
                && bytes[18] as usize == bytes.len() - 19
                && bytes[18] as usize <= DATA_BYTES =>
            {
                Self::Header {
                    package,
                    offset: bytes[17],
                    data: &bytes[19..],
                }
            }
            1 if bytes.len() == 17 => Self::Prepare { package },
            2 if bytes.len() >= 23
                && bytes[21] as usize == bytes.len() - 22
                && bytes[21] as usize <= DATA_BYTES =>
            {
                Self::Data {
                    package,
                    offset: u32::from_le_bytes(bytes[17..21].try_into().ok()?),
                    data: &bytes[22..],
                }
            }
            3 if bytes.len() == 17 => Self::Finish { package },
            4 if bytes.len() == 17 => Self::Status { package },
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Idle = 0,
    Header = 1,
    Preparing = 2,
    Receiving = 3,
    Ready = 4,
    Fault = 5,
    Selected = 6,
    Verifying = 7,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    Unsupported = 1,
    Invalid = 2,
    Signature = 3,
    Target = 4,
    Version = 5,
    Busy = 6,
    Order = 7,
    Storage = 8,
    Verification = 9,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub package: PackageId,
    pub phase: Phase,
    pub status: Status,
    /// Header bytes accepted while assembling; image bytes accepted afterward.
    pub accepted: u32,
    /// Image bytes retained by a completed flash checkpoint, not merely MAC receipt.
    pub committed: u32,
    /// Staging pages erased this boot; retained after preparation completes.
    pub prepared: u16,
}
impl Progress {
    pub fn unsupported(package: PackageId) -> Self {
        Self {
            package,
            phase: Phase::Idle,
            status: Status::Unsupported,
            accepted: 0,
            committed: 0,
            prepared: 0,
        }
    }
    pub fn encode(self, out: &mut [u8; PROGRESS_BYTES]) {
        out[..16].copy_from_slice(&self.package);
        out[16] = self.phase as u8;
        out[17] = self.status as u8;
        out[18..22].copy_from_slice(&self.accepted.to_le_bytes());
        out[22..26].copy_from_slice(&self.committed.to_le_bytes());
        out[26..28].copy_from_slice(&self.prepared.to_le_bytes());
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != PROGRESS_BYTES {
            return None;
        }
        let phase = match bytes[16] {
            0 => Phase::Idle,
            1 => Phase::Header,
            2 => Phase::Preparing,
            3 => Phase::Receiving,
            4 => Phase::Ready,
            5 => Phase::Fault,
            6 => Phase::Selected,
            7 => Phase::Verifying,
            _ => return None,
        };
        let status = match bytes[17] {
            0 => Status::Ok,
            1 => Status::Unsupported,
            2 => Status::Invalid,
            3 => Status::Signature,
            4 => Status::Target,
            5 => Status::Version,
            6 => Status::Busy,
            7 => Status::Order,
            8 => Status::Storage,
            9 => Status::Verification,
            _ => return None,
        };
        Some(Self {
            package: bytes[..16].try_into().ok()?,
            phase,
            status,
            accepted: u32::from_le_bytes(bytes[18..22].try_into().ok()?),
            committed: u32::from_le_bytes(bytes[22..26].try_into().ok()?),
            prepared: u16::from_le_bytes(bytes[26..28].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_records_reject_truncation_and_trailing_bytes() {
        let package = [9; 16];
        for request in [
            Request::Header {
                package,
                offset: 144,
                data: &[255; DATA_BYTES],
            },
            Request::Prepare { package },
            Request::Data {
                package,
                offset: u32::MAX,
                data: &[255; DATA_BYTES],
            },
            Request::Finish { package },
            Request::Status { package },
        ] {
            let mut bytes = [0; REQUEST_BYTES];
            let n = request.encode(&mut bytes);
            assert_eq!(Request::decode(&bytes[..n]), Some(request));
            for end in 0..n {
                assert_eq!(Request::decode(&bytes[..end]), None);
            }
            let mut extra = [0; REQUEST_BYTES + 1];
            extra[..n].copy_from_slice(&bytes[..n]);
            assert_eq!(Request::decode(&extra[..n + 1]), None);
        }
        let progress = Progress {
            package,
            phase: Phase::Verifying,
            status: Status::Ok,
            accepted: u32::MAX,
            committed: u32::MAX,
            prepared: u16::MAX,
        };
        let mut bytes = [0; PROGRESS_BYTES];
        progress.encode(&mut bytes);
        assert_eq!(Progress::decode(&bytes), Some(progress));
        bytes[16] = 255;
        assert_eq!(Progress::decode(&bytes), None);
        bytes[16] = Phase::Ready as u8;
        bytes[17] = 255;
        assert_eq!(Progress::decode(&bytes), None);
    }
}
