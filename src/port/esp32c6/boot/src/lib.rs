#![no_std]
// SPDX-License-Identifier: MIT
//! ESP32-C6 inactive-image storage and ESP-IDF trial/confirmation metadata.
//! The vendor bootloader performs selection and rollback; no vendor runtime is
//! linked here. The port supplies the actual running slot from its MMU mapping.

mod metadata;
use embedded_storage::nor_flash::{NorFlash, NorFlashError, NorFlashErrorKind};
use lattice_storage::{Candidate, Error as StorageError};
pub use metadata::BootState;
use metadata::Record;

pub const SECTOR_SIZE: u32 = 4096;
#[repr(align(4))]
struct Aligned<const N: usize>([u8; N]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub offset: u32,
    pub length: u32,
}
impl Region {
    fn end(self) -> Option<u32> {
        self.offset.checked_add(self.length)
    }
    fn valid(self, capacity: usize) -> bool {
        self.offset >= 0x9000
            && self.offset.is_multiple_of(SECTOR_SIZE)
            && self.length != 0
            && self.length.is_multiple_of(SECTOR_SIZE)
            && self.end().is_some_and(|end| end as usize <= capacity)
    }
    fn overlaps(self, other: Self) -> bool {
        self.offset < other.end().unwrap_or(u32::MAX)
            && other.offset < self.end().unwrap_or(u32::MAX)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub ota_data: Region,
    pub journal: Option<Region>,
    pub configuration: Option<Region>,
    pub images: [Region; 2],
}
impl Layout {
    pub fn valid(self, capacity: usize) -> bool {
        let [a, b] = self.images;
        self.ota_data.valid(capacity)
            && self.ota_data.length == 2 * SECTOR_SIZE
            && a.valid(capacity)
            && b.valid(capacity)
            && a.offset.is_multiple_of(0x10000)
            && b.offset.is_multiple_of(0x10000)
            && !a.overlaps(b)
            && !a.overlaps(self.ota_data)
            && !b.overlaps(self.ota_data)
            && self.journal.is_none_or(|j| {
                j.valid(capacity) && !j.overlaps(a) && !j.overlaps(b) && !j.overlaps(self.ota_data)
            })
            && self.configuration.is_none_or(|c| {
                c.valid(capacity)
                    && c.length.is_multiple_of(2 * SECTOR_SIZE)
                    && !c.overlaps(a)
                    && !c.overlaps(b)
                    && !c.overlaps(self.ota_data)
                    && self.journal.is_none_or(|j| !c.overlaps(j))
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Layout,
    Metadata,
    RunningSlot,
    RollbackDisabled,
    SequenceExhausted,
    Bounds,
    Order,
    TrialPending,
    Incomplete,
    Digest,
    Image,
    Flash(NorFlashErrorKind),
}
fn flash_error(error: impl NorFlashError) -> Error {
    Error::Flash(error.kind())
}

pub struct ImageStorage<F: NorFlash> {
    flash: F,
    layout: Layout,
    running: usize,
    records: [Option<Record>; 2],
    record_index: usize,
    selected: bool,
    candidate: Option<Candidate<F>>,
}

impl From<StorageError> for Error {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Bounds => Self::Bounds,
            StorageError::Order => Self::Order,
            StorageError::Incomplete => Self::Incomplete,
            StorageError::Digest => Self::Digest,
            StorageError::Flash(error) => Self::Flash(error),
        }
    }
}

impl<F: NorFlash> ImageStorage<F> {
    pub fn new(mut flash: F, layout: Layout, running: usize) -> Result<Self, Error> {
        if !layout.valid(flash.capacity())
            || F::ERASE_SIZE != SECTOR_SIZE as usize
            || F::WRITE_SIZE != 4
            || F::READ_SIZE != 1
        {
            return Err(Error::Layout);
        }
        if running >= 2 {
            return Err(Error::RunningSlot);
        }
        let records = metadata::read(&mut flash, layout.ota_data.offset)?;
        let record_index = metadata::selected(&records).ok_or(Error::Metadata)?;
        let record = records[record_index].unwrap();
        if record.slot() != running {
            return Err(Error::RunningSlot);
        }
        // With rollback enabled, the loader changes New to PendingVerify before entry.
        if record.state == BootState::New {
            return Err(Error::RollbackDisabled);
        }
        Ok(Self {
            flash,
            layout,
            running,
            records,
            record_index,
            selected: false,
            candidate: None,
        })
    }

    pub fn running_slot(&self) -> usize {
        self.running
    }
    pub fn boot_state(&self) -> BootState {
        self.records[self.record_index].unwrap().state
    }
    pub fn image_capacity(&self) -> u32 {
        self.layout.images[1 - self.running].length
    }
    pub fn configuration(&mut self) -> Result<lattice_storage::region::Region<'_, F>, Error> {
        let read_only = self.selected || self.boot_state() == BootState::PendingVerify;
        let region = self.layout.configuration.ok_or(Error::Layout)?;
        let region =
            lattice_storage::region::Region::new(&mut self.flash, region.offset, region.length)
                .map_err(Error::Flash)?;
        Ok(if read_only {
            region.read_only()
        } else {
            region
        })
    }

    pub fn confirm(&mut self) -> Result<(), Error> {
        if self.selected {
            return Err(Error::TrialPending);
        }
        if self.boot_state() == BootState::Valid {
            return Ok(());
        }
        let mut record = self.records[self.record_index].unwrap();
        record.state = BootState::Valid;
        self.selected = true; // lock further writes if metadata mutation fails
        metadata::write(
            &mut self.flash,
            self.layout.ota_data.offset + self.record_index as u32 * SECTOR_SIZE,
            record,
        )?;
        self.records[self.record_index] = Some(record);
        self.selected = false;
        Ok(())
    }

    pub fn begin(&mut self, length: u32) -> Result<(), Error> {
        if self.selected || self.boot_state() == BootState::PendingVerify {
            return Err(Error::TrialPending);
        }
        if !(24..=self.image_capacity()).contains(&length) {
            return Err(Error::Bounds);
        }
        let image = self.layout.images[1 - self.running];
        self.candidate = Some(Candidate::new(
            &self.flash,
            image.offset,
            image.length,
            length,
            length.div_ceil(SECTOR_SIZE) * SECTOR_SIZE,
        )?);
        Ok(())
    }

    /// Erase one sector needed by the new image. Unlike a swap-based loader,
    /// ESP-IDF does not copy the remainder of the slot into the active image.
    pub fn erase_next_page(&mut self) -> Result<bool, Error> {
        self.candidate
            .as_mut()
            .ok_or(Error::Order)?
            .erase_next_page(&mut self.flash)
            .map_err(Into::into)
    }

    pub fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Error> {
        self.candidate
            .as_mut()
            .ok_or(Error::Order)?
            .write(&mut self.flash, offset, bytes)
            .map_err(Into::into)
    }

    /// Verify staged bytes and the ESP image header without selecting a trial.
    /// Release signature authorization belongs to the common update service.
    pub fn verify_image(&mut self, expected_sha256: &[u8; 32]) -> Result<(), Error> {
        while !self.verify_step(expected_sha256, 1024)? {}
        Ok(())
    }

    pub fn verify_step(&mut self, expected_sha256: &[u8; 32], bytes: u32) -> Result<bool, Error> {
        let candidate = self.candidate.as_mut().ok_or(Error::Order)?;
        if !candidate.verify_step(&mut self.flash, expected_sha256, bytes)? {
            return Ok(false);
        }
        let image = self.layout.images[1 - self.running];
        let mut header = Aligned([0; 24]);
        self.flash
            .read(image.offset, &mut header.0)
            .map_err(flash_error)?;
        if header.0[0] != 0xe9
            || !(1..=16).contains(&header.0[1])
            || u16::from_le_bytes(header.0[12..14].try_into().unwrap()) != 13
            || header.0[23] > 1
        {
            return Err(Error::Image);
        }
        Ok(true)
    }
    /// Require the complete digest and valid header before changing boot state.
    /// An unchanged verified candidate reuses its digest rather than rereading it.
    pub fn select_trial(&mut self, expected_sha256: &[u8; 32]) -> Result<(), Error> {
        self.verify_image(expected_sha256)?;
        let record = Record {
            sequence: metadata::next_sequence(&self.records, 1 - self.running)?,
            state: BootState::New,
        };
        let destination = 1 - self.record_index;
        self.selected = true;
        self.candidate = None;
        metadata::write(
            &mut self.flash,
            self.layout.ota_data.offset + destination as u32 * SECTOR_SIZE,
            record,
        )
    }
    pub fn resume(&mut self, length: u32, completed: u32) -> Result<(), Error> {
        self.begin(length)?;
        self.candidate
            .as_mut()
            .unwrap()
            .resume(completed)
            .map_err(Into::into)
    }
    pub fn journal_capacity(&self) -> u32 {
        self.layout.journal.map_or(0, |j| j.length)
    }
    pub fn journal_read(&mut self, offset: u32, data: &mut [u8]) -> Result<(), Error> {
        let j = self.layout.journal.ok_or(Error::Layout)?;
        if offset
            .checked_add(data.len() as u32)
            .is_none_or(|end| end > j.length)
        {
            return Err(Error::Bounds);
        }
        self.flash
            .read(j.offset + offset, data)
            .map_err(flash_error)
    }
    pub fn journal_erase(&mut self, page: u32) -> Result<(), Error> {
        if self.selected || self.boot_state() == BootState::PendingVerify {
            return Err(Error::TrialPending);
        }
        let j = self.layout.journal.ok_or(Error::Layout)?;
        let offset = page.checked_mul(SECTOR_SIZE).ok_or(Error::Bounds)?;
        if offset
            .checked_add(SECTOR_SIZE)
            .is_none_or(|end| end > j.length)
        {
            return Err(Error::Bounds);
        }
        self.flash
            .erase(j.offset + offset, j.offset + offset + SECTOR_SIZE)
            .map_err(flash_error)
    }
    pub fn journal_write(&mut self, offset: u32, data: &[u8]) -> Result<(), Error> {
        if self.selected || self.boot_state() == BootState::PendingVerify {
            return Err(Error::TrialPending);
        }
        let j = self.layout.journal.ok_or(Error::Layout)?;
        if data.len() > 256
            || !offset.is_multiple_of(4)
            || !data.len().is_multiple_of(4)
            || offset
                .checked_add(data.len() as u32)
                .is_none_or(|end| end > j.length)
        {
            return Err(Error::Bounds);
        }
        let mut aligned = Aligned([0; 256]);
        aligned.0[..data.len()].copy_from_slice(data);
        self.flash
            .write(j.offset + offset, &aligned.0[..data.len()])
            .map_err(flash_error)
    }
}

#[cfg(test)]
mod tests;
