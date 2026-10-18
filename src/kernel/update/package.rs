//! Signed, fixed-size release header. No executable metadata or IR interpreter.
use ed25519_dalek::{Signature, VerifyingKey};
use lattice_peer_wire::update::{PackageId, Status};
use sha2::{Digest, Sha256};

pub const HEADER_BYTES: usize = 192;
pub const SIGNED_BYTES: usize = 128;
const MAGIC: &[u8; 4] = b"LVUP";

/// Provisioned release identity. Keys and compatibility are never learned from
/// a package or its transmitting peer. These are firmware-build facts, not radio settings.
#[derive(Clone, Copy)]
pub struct Policy {
    pub public_key: [u8; 32],
    pub platform: u16,
    pub layout: u32,
    pub appliance: [u8; 16],
    pub surface: [u8; 32],
    pub release: u64,
}

/// Local dry-run coefficients. Per-byte fields are unsigned Q24.8 microseconds.
/// MAX denotes unknown, including when no release dry run has measured a term.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Costs {
    pub erase_page_us: u32,
    pub write_per_byte: u32,
    pub verify_per_byte: u32,
    pub activation_us: u32,
    pub swap_per_byte: u32,
    pub longest_pause_us: u32,
}
impl Costs {
    pub fn staging_us(self, bytes: u32, pages: u32) -> Option<u64> {
        if [
            self.erase_page_us,
            self.write_per_byte,
            self.verify_per_byte,
        ]
        .contains(&u32::MAX)
        {
            return None;
        }
        let erase = self.erase_page_us as u128 * pages as u128;
        let bytes = ((self.write_per_byte as u128 + self.verify_per_byte as u128) * bytes as u128)
            .div_ceil(256);
        u64::try_from(erase + bytes).ok()
    }
    pub fn activation_us(self, swap_bytes: u32) -> Option<u64> {
        if self.activation_us == u32::MAX || self.swap_per_byte == u32::MAX {
            return None;
        }
        Some(
            self.activation_us as u64
                + (self.swap_per_byte as u64 * swap_bytes as u64).div_ceil(256),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub id: PackageId,
    pub image_bytes: u32,
    pub image_sha256: [u8; 32],
    pub costs: Costs,
}

pub fn identity(bytes: &[u8; HEADER_BYTES]) -> PackageId {
    Sha256::digest(bytes)[..16].try_into().unwrap()
}

pub fn verify(
    bytes: &[u8; HEADER_BYTES],
    policy: &Policy,
    capacity: u32,
) -> Result<Header, Status> {
    if &bytes[..4] != MAGIC || bytes[4..6] != [1, 0] {
        return Err(Status::Invalid);
    }
    let key = VerifyingKey::from_bytes(&policy.public_key).map_err(|_| Status::Signature)?;
    let signature = Signature::from_bytes(bytes[SIGNED_BYTES..].try_into().unwrap());
    key.verify_strict(&bytes[..SIGNED_BYTES], &signature)
        .map_err(|_| Status::Signature)?;
    if u16::from_le_bytes(bytes[6..8].try_into().unwrap()) != policy.platform
        || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) != policy.layout
        || bytes[12..28] != policy.appliance
        || bytes[28..60] != policy.surface
    {
        return Err(Status::Target);
    }
    if u64::from_le_bytes(bytes[60..68].try_into().unwrap()) <= policy.release {
        return Err(Status::Version);
    }
    let image_bytes = u32::from_le_bytes(bytes[68..72].try_into().unwrap());
    if image_bytes == 0 || image_bytes > capacity {
        return Err(Status::Target);
    }
    let field = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    Ok(Header {
        id: identity(bytes),
        image_bytes,
        image_sha256: bytes[72..104].try_into().unwrap(),
        costs: Costs {
            erase_page_us: field(104),
            write_per_byte: field(108),
            verify_per_byte: field(112),
            activation_us: field(116),
            swap_per_byte: field(120),
            longest_pause_us: field(124),
        },
    })
}
