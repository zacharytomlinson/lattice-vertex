//! Signed header followed by append-only offset/complement checkpoints. A torn
//! record is skipped, never programmed over. Image digest checking remains final.
use super::{
    package::{self, Header, Policy, HEADER_BYTES},
    Storage, PAGE_BYTES,
};
use lattice_peer_wire::update::Status;

pub(super) const RECORDS: u32 = 256;
const COMMIT: u32 = 192;
const CONSUMED: u32 = 196;
const MAGIC: [u8; 4] = *b"LUC1";
pub(super) struct Saved {
    pub header: Header,
    pub committed: u32,
    pub next: u32,
}

pub(super) fn initialize<S: Storage>(
    storage: &mut S,
    header: &[u8; HEADER_BYTES],
) -> Result<(), S::Error> {
    storage.journal_write(0, header)?;
    storage.journal_write(COMMIT, &MAGIC)
}
pub(super) fn consume<S: Storage>(storage: &mut S) -> Result<(), S::Error> {
    storage.journal_write(CONSUMED, &[0; 4])
}
pub(super) fn checkpoint<S: Storage>(
    storage: &mut S,
    offset: u32,
    completed: u32,
) -> Result<(), S::Error> {
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&completed.to_le_bytes());
    bytes[4..].copy_from_slice(&(!completed).to_le_bytes());
    storage.journal_write(offset, &bytes)
}
pub(super) fn load<S: Storage>(storage: &mut S, policy: &Policy) -> Result<Option<Saved>, Status> {
    if storage.journal_capacity() < RECORDS {
        return Ok(None);
    }
    let mut marker = [0; 8];
    storage
        .journal_read(COMMIT, &mut marker)
        .map_err(|_| Status::Storage)?;
    if marker[..4] != MAGIC || marker[4..] != [0xff; 4] {
        return Ok(None);
    }
    let mut bytes = [0; HEADER_BYTES];
    storage
        .journal_read(0, &mut bytes)
        .map_err(|_| Status::Storage)?;
    let Ok(header) = package::verify(&bytes, policy, storage.capacity()) else {
        return Ok(None);
    };
    let mut committed = 0;
    let mut next = RECORDS;
    while next + 8 <= storage.journal_capacity() {
        let mut entry = [0; 8];
        storage
            .journal_read(next, &mut entry)
            .map_err(|_| Status::Storage)?;
        if entry == [0xff; 8] {
            break;
        }
        let offset = u32::from_le_bytes(entry[..4].try_into().unwrap());
        let inverse = u32::from_le_bytes(entry[4..].try_into().unwrap());
        if offset ^ inverse == u32::MAX
            && offset > committed
            && offset <= header.image_bytes
            && (offset.is_multiple_of(PAGE_BYTES) || offset == header.image_bytes)
        {
            committed = offset;
        }
        next += 8;
    }
    Ok(Some(Saved {
        header,
        committed,
        next,
    }))
}
