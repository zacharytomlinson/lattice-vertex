//! One signed candidate, bounded flash work, and page-level durable progress.
//! Ordinary peer dispatch owns the receiver. Local activity and signed timing
//! govern activation; a transmitting peer has no reboot command.

pub(crate) mod activation;
pub use activation::{hold_activation, ActivationHold};
mod journal;
pub mod package;
#[cfg(any(
    all(feature = "port-nrf52840", feature = "nrf52840-update"),
    all(feature = "port-esp32c6", feature = "esp32c6-update")
))]
pub(crate) mod runtime;
#[cfg(any(
    all(feature = "port-nrf52840", feature = "nrf52840-update"),
    all(feature = "port-esp32c6", feature = "esp32c6-update")
))]
pub use runtime::{confirm, select_trial, snapshot, PlatformStorage};

use lattice_peer_wire::update::{PackageId, Request};
pub use lattice_peer_wire::update::{Phase, Progress, Status};
use package::{Header, Policy, HEADER_BYTES};

pub trait Storage {
    type Error;
    fn capacity(&self) -> u32;
    fn journal_capacity(&self) -> u32;
    fn journal_read(&mut self, offset: u32, data: &mut [u8]) -> Result<(), Self::Error>;
    fn journal_erase(&mut self, page: u32) -> Result<(), Self::Error>;
    fn journal_write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error>;
    fn resume(&mut self, length: u32, completed: u32) -> Result<(), Self::Error>;
    fn erase_next_page(&mut self) -> Result<bool, Self::Error>;
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error>;
    fn verify(&mut self, digest: &[u8; 32]) -> Result<(), Self::Error>;
    fn select_trial(&mut self, digest: &[u8; 32]) -> Result<(), Self::Error>;
    fn confirm(&mut self) -> Result<(), Self::Error>;
    /// Bytes copied by the bootloader during activation; zero for direct slot selection.
    fn activation_swap_bytes(&self) -> u32 {
        0
    }
}

/// Both current ports use 4 KiB erase pages. Geometry remains a port-owned fact;
/// expanding this shared format to other page sizes requires an explicit revision.
pub const PAGE_BYTES: u32 = 4096;

/// Fingerprint the actual exposed definitions, including their local handles.
/// Packaging must preserve this surface in the candidate for this initial ABI.
pub fn surface_identity<A: 'static, E: 'static>(
    surface: &'static crate::surface::Surface<'static, A, E>,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let description =
        crate::kernel::peer::exposure::Description::new(surface).expect("invalid update surface");
    let mut hash = Sha256::new();
    let mut part = [0; 64];
    let mut offset = 0;
    while offset < description.len() {
        let n = description.encode_part(offset, &mut part);
        hash.update(&part[..n]);
        offset += n as u16;
    }
    hash.finalize().into()
}

pub struct Receiver<S: Storage> {
    storage: S,
    policy: Policy,
    header: Option<Header>,
    phase: Phase,
    accepted: u32,
    committed: u32,
    next_record: u32,
    journal_pages: u32,
    initialize_journal: bool,
    prepared: u16,
    assembling: PackageId,
    header_bytes: [u8; HEADER_BYTES],
    header_length: usize,
}

impl<S: Storage> Receiver<S> {
    pub fn new(storage: S, policy: Policy) -> Self {
        let mut this = Self {
            storage,
            policy,
            header: None,
            phase: Phase::Idle,
            accepted: 0,
            committed: 0,
            next_record: journal::RECORDS,
            journal_pages: 0,
            initialize_journal: false,
            prepared: 0,
            assembling: [0; 16],
            header_bytes: [0; HEADER_BYTES],
            header_length: 0,
        };
        // Recovery verifies the stored signature and applicability again. Invalid
        // or consumed metadata does not authorize any image writes or activation.
        let _ = this.recover(None);
        this
    }

    pub fn progress(&self, package: PackageId, status: Status) -> Progress {
        if self.header.is_some_and(|h| h.id == package) {
            Progress {
                package,
                phase: self.phase,
                status,
                accepted: self.accepted,
                committed: self.committed,
                prepared: self.prepared,
            }
        } else {
            Progress {
                package,
                phase: if self.assembling == package && self.header_length != 0 {
                    Phase::Header
                } else {
                    Phase::Idle
                },
                status,
                accepted: if self.assembling == package {
                    self.header_length as u32
                } else {
                    0
                },
                committed: 0,
                prepared: 0,
            }
        }
    }

    pub fn handle(&mut self, request: Request<'_>) -> Progress {
        let package = request.package();
        let result = match request {
            Request::Header { offset, data, .. } => {
                self.accept_header(package, offset as usize, data)
            }
            _ if !self.header.is_some_and(|h| h.id == package) => Err(Status::Order),
            Request::Prepare { .. } => self.prepare(),
            Request::Data { offset, data, .. } => self.write(offset, data),
            Request::Finish { .. } => self.finish(),
            Request::Status { .. } => Ok(()),
        };
        self.progress(package, result.err().unwrap_or(Status::Ok))
    }

    fn accept_header(&mut self, id: PackageId, offset: usize, data: &[u8]) -> Result<(), Status> {
        if data.is_empty()
            || offset
                .checked_add(data.len())
                .is_none_or(|n| n > HEADER_BYTES)
        {
            return Err(Status::Invalid);
        }
        // Until the authenticated header is durable, its buffer belongs to
        // journal initialization. Another offer cannot replace those bytes.
        if self.initialize_journal
            && self.phase == Phase::Preparing
            && self.header.is_some_and(|h| h.id != id)
        {
            return Err(Status::Busy);
        }
        if self.header.is_some_and(|h| h.id == id) && self.phase != Phase::Fault {
            return Ok(());
        }
        if offset == 0 && (self.assembling != id || self.header_length == HEADER_BYTES) {
            self.assembling = id;
            self.header_length = 0;
        }
        if self.assembling != id {
            return Err(Status::Order);
        }
        if offset + data.len() <= self.header_length {
            return if &self.header_bytes[offset..offset + data.len()] == data {
                Ok(())
            } else {
                Err(Status::Order)
            };
        }
        if offset != self.header_length {
            return Err(Status::Order);
        }
        self.header_bytes[offset..offset + data.len()].copy_from_slice(data);
        self.header_length += data.len();
        if self.header_length != HEADER_BYTES {
            return Ok(());
        }
        let header = package::verify(&self.header_bytes, &self.policy, self.storage.capacity())?;
        if header.id != id {
            return Err(Status::Invalid);
        }
        if self.recover(Some(id))? {
            return Ok(());
        }
        if self.storage.journal_capacity()
            < journal::RECORDS + 8 * (header.image_bytes.div_ceil(PAGE_BYTES) + 1)
            || !self.storage.journal_capacity().is_multiple_of(PAGE_BYTES)
        {
            return Err(Status::Storage);
        }
        // Check trial protection before touching the old journal. This only
        // establishes volatile staging bounds; it does not erase image bytes.
        self.storage
            .resume(header.image_bytes, 0)
            .map_err(|_| Status::Storage)?;
        self.header = Some(header);
        self.phase = Phase::Preparing;
        self.accepted = 0;
        self.committed = 0;
        self.next_record = journal::RECORDS;
        self.journal_pages = 0;
        self.initialize_journal = true;
        self.prepared = 0;
        Ok(())
    }

    fn recover(&mut self, expected: Option<PackageId>) -> Result<bool, Status> {
        let Some(saved) = journal::load(&mut self.storage, &self.policy)? else {
            return Ok(false);
        };
        if expected.is_some_and(|id| id != saved.header.id) {
            return Ok(false);
        }
        self.storage
            .resume(saved.header.image_bytes, saved.committed)
            .map_err(|_| Status::Storage)?;
        self.header = Some(saved.header);
        self.accepted = saved.committed;
        self.committed = saved.committed;
        self.next_record = saved.next;
        self.initialize_journal = false;
        self.prepared = 0;
        self.phase = if self.accepted == saved.header.image_bytes {
            Phase::Receiving
        } else {
            Phase::Preparing
        };
        Ok(true)
    }

    fn prepare(&mut self) -> Result<(), Status> {
        if matches!(self.phase, Phase::Receiving | Phase::Ready) {
            return Ok(());
        }
        if self.phase != Phase::Preparing {
            return Err(Status::Order);
        }
        let result = if self.initialize_journal {
            if self.journal_pages * PAGE_BYTES < self.storage.journal_capacity() {
                self.storage
                    .journal_erase(self.journal_pages)
                    .map(|()| self.journal_pages += 1)
            } else {
                let r = journal::initialize(&mut self.storage, &self.header_bytes);
                if r.is_ok() {
                    self.initialize_journal = false;
                }
                r
            }
        } else {
            self.storage.erase_next_page().map(|complete| {
                self.prepared = self.prepared.saturating_add(1);
                if complete {
                    self.phase = Phase::Receiving;
                }
            })
        };
        if result.is_err() {
            self.phase = Phase::Fault;
            return Err(Status::Storage);
        }
        Ok(())
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Status> {
        if self.phase != Phase::Receiving {
            return Err(Status::Order);
        }
        let header = self.header.unwrap();
        let end = offset
            .checked_add(data.len() as u32)
            .ok_or(Status::Invalid)?;
        if end > header.image_bytes
            || offset > self.accepted
            || (offset < self.accepted && end > self.accepted)
        {
            return Err(Status::Order);
        }
        if self.storage.write(offset, data).is_err() {
            self.phase = Phase::Fault;
            return Err(Status::Storage);
        }
        self.accepted = self.accepted.max(end);
        let completed = if self.accepted == header.image_bytes {
            self.accepted
        } else {
            self.accepted / PAGE_BYTES * PAGE_BYTES
        };
        if completed > self.committed {
            if journal::checkpoint(&mut self.storage, self.next_record, completed).is_err() {
                self.phase = Phase::Fault;
                return Err(Status::Storage);
            }
            self.next_record += 8;
            self.committed = completed;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Status> {
        if self.phase == Phase::Ready {
            return Ok(());
        }
        let header = self.header.ok_or(Status::Order)?;
        if self.phase != Phase::Receiving || self.accepted != header.image_bytes {
            return Err(Status::Order);
        }
        if self.storage.verify(&header.image_sha256).is_err() {
            self.phase = Phase::Fault;
            journal::consume(&mut self.storage).map_err(|_| Status::Storage)?;
            return Err(Status::Verification);
        }
        self.phase = Phase::Ready;
        Ok(())
    }

    /// Called locally after an activation decision. No radio operation
    /// invokes this. A consumed journal cannot resurrect a reverted trial.
    pub fn activate(&mut self) -> Result<(), Status> {
        if self.phase != Phase::Ready {
            return Err(Status::Order);
        }
        self.phase = Phase::Fault;
        journal::consume(&mut self.storage).map_err(|_| Status::Storage)?;
        self.storage
            .select_trial(&self.header.unwrap().image_sha256)
            .map_err(|_| Status::Storage)?;
        self.phase = Phase::Selected;
        Ok(())
    }
    pub fn confirm(&mut self) -> Result<(), Status> {
        self.storage.confirm().map_err(|_| Status::Storage)
    }
}

#[cfg(test)]
mod tests;
