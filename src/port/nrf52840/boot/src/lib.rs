#![no_std]
//! Nordic flash staging and Embassy trial/rollback integration.
//!
//! No transport, scheduler, or appliance definitions live in this crate. The
//! separately linked loader and the kernel port use these same flash boundaries.
//! The common updater owns package authorization, durable transfer progress and
//! activation. This crate owns the flash regions, persistent boot state, and
//! loader watchdog handoff.

pub mod layout;
#[cfg(feature = "loader")]
pub mod watchdog;

use core::cell::RefCell;
use embassy_boot::{AlignedBuffer, BlockingFirmwareState, BootLoader, BootLoaderConfig, State};
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_sync::blocking_mutex::{raw::NoopRawMutex, Mutex};
use embedded_storage::nor_flash::{NorFlash, NorFlashError, NorFlashErrorKind};
use lattice_storage::{Candidate, Error as StorageError};
use layout::{Layout, PAGE_SIZE, WRITE_SIZE};

pub use embassy_boot::State as BootState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Layout,
    FactoryCrc,
    Bounds,
    Order,
    TrialPending,
    Incomplete,
    Digest,
    Vectors,
    Flash(NorFlashErrorKind),
    Boot,
}

fn flash_error(e: impl NorFlashError) -> Error {
    Error::Flash(e.kind())
}

type FlashCell<F> = Mutex<NoopRawMutex, RefCell<F>>;
type Partition<'a, F> = BlockingPartition<'a, NoopRawMutex, F>;

fn partition<F: NorFlash>(flash: &FlashCell<F>, start: u32, end: u32) -> Partition<'_, F> {
    BlockingPartition::new(flash, start, end - start)
}

fn validate<F: NorFlash>(flash: &F, layout: Layout) -> Result<(), Error> {
    if !layout.valid()
        || layout.end as usize > flash.capacity()
        || F::WRITE_SIZE != WRITE_SIZE
        || F::ERASE_SIZE != PAGE_SIZE as usize
        || F::READ_SIZE != 1
    {
        return Err(Error::Layout);
    }
    Ok(())
}

/// One candidate in the inactive slot. Owns the flash peripheral exclusively.
/// Calls must run in task/startup context; page erases can stall flash execution.
pub struct ImageStorage<F: NorFlash> {
    flash: FlashCell<F>,
    layout: Layout,
    boot: BootState,
    candidate: Option<Candidate<F>>,
    selected: bool,
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
    pub fn new(flash: F, layout: Layout) -> Result<Self, Error> {
        validate(&flash, layout)?;
        let mut this = Self {
            flash: Mutex::new(RefCell::new(flash)),
            layout,
            boot: State::Boot,
            candidate: None,
            selected: false,
        };
        this.boot = this.with_state(|s| s.get_state())?;
        Ok(this)
    }

    pub fn boot_state(&self) -> &BootState {
        &self.boot
    }
    pub fn image_capacity(&self) -> u32 {
        self.layout.image_capacity()
    }

    fn with_state<T>(
        &mut self,
        op: impl FnOnce(
            &mut BlockingFirmwareState<'_, Partition<'_, F>>,
        ) -> Result<T, embassy_boot::FirmwareUpdaterError>,
    ) -> Result<T, Error> {
        let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
        let mut state = BlockingFirmwareState::new(
            partition(
                &self.flash,
                self.layout.state_start,
                self.layout.active_start,
            ),
            aligned.as_mut(),
        );
        op(&mut state).map_err(|_| Error::Boot)
    }

    /// Confirm this boot after local initialization succeeds. Never cancels an
    /// update selected during this same boot.
    pub fn confirm(&mut self) -> Result<(), Error> {
        if self.selected {
            return Err(Error::TrialPending);
        }
        self.with_state(|s| s.mark_booted())?;
        self.boot = State::Boot;
        Ok(())
    }

    /// Start/restart a candidate. An unconfirmed trial must retain its rollback image.
    pub fn begin(&mut self, length: u32) -> Result<(), Error> {
        if self.boot == State::Swap || self.selected {
            return Err(Error::TrialPending);
        }
        if !(8..=self.image_capacity()).contains(&length) {
            return Err(Error::Bounds);
        }
        let capacity = self.image_capacity();
        self.candidate = Some(Candidate::new(
            self.flash.get_mut().get_mut(),
            self.layout.dfu_start,
            capacity,
            length,
            capacity,
        )?);
        Ok(())
    }

    /// Erase one candidate page. Returns true when all image pages are erased.
    /// The extra DFU swap page is exclusively managed by the bootloader.
    pub fn erase_next_page(&mut self) -> Result<bool, Error> {
        self.candidate
            .as_mut()
            .ok_or(Error::Order)?
            .erase_next_page(self.flash.get_mut().get_mut())
            .map_err(Into::into)
    }

    /// Append at most 256 bytes; offsets and non-final lengths are word aligned.
    /// An identical already-written range is accepted without another flash write.
    pub fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Error> {
        self.candidate
            .as_mut()
            .ok_or(Error::Order)?
            .write(self.flash.get_mut().get_mut(), offset, bytes)
            .map_err(Into::into)
    }

    /// Verify staged bytes and vectors without changing boot selection.
    /// Release signature authorization belongs to the common update service.
    pub fn verify_image(&mut self, expected_sha256: &[u8; 32]) -> Result<(), Error> {
        while !self.verify_step(expected_sha256, 1024)? {}
        Ok(())
    }

    pub fn verify_step(&mut self, expected_sha256: &[u8; 32], bytes: u32) -> Result<bool, Error> {
        let candidate = self.candidate.as_mut().ok_or(Error::Order)?;
        let flash = self.flash.get_mut().get_mut();
        if !candidate.verify_step(flash, expected_sha256, bytes)? {
            return Ok(false);
        }
        let mut vectors = [0; 8];
        flash
            .read(self.layout.dfu_start, &mut vectors)
            .map_err(flash_error)?;
        if !self.layout.valid_vectors(candidate.length(), &vectors) {
            return Err(Error::Vectors);
        }
        Ok(true)
    }

    /// Require the complete digest and valid vectors before changing boot state.
    /// An unchanged verified candidate reuses its digest rather than rereading it.
    pub fn select_trial(&mut self, expected_sha256: &[u8; 32]) -> Result<(), Error> {
        self.verify_image(expected_sha256)?;
        self.selected = true;
        self.candidate = None;
        self.with_state(|s| s.mark_updated())
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
        self.layout.configuration_start - self.layout.journal_start
    }
    /// Borrow the protected region through the existing flash owner. Boot swap,
    /// image staging and transfer-journal methods never address this region.
    pub fn configuration(&mut self) -> Result<lattice_storage::region::Region<'_, F>, Error> {
        let read_only = self.selected || self.boot == State::Swap;
        let region = lattice_storage::region::Region::new(
            self.flash.get_mut().get_mut(),
            self.layout.configuration_start,
            self.layout.end - self.layout.configuration_start,
        )
        .map_err(Error::Flash)?;
        Ok(if read_only {
            region.read_only()
        } else {
            region
        })
    }
    pub fn journal_read(&mut self, offset: u32, data: &mut [u8]) -> Result<(), Error> {
        if offset
            .checked_add(data.len() as u32)
            .is_none_or(|end| end > self.journal_capacity())
        {
            return Err(Error::Bounds);
        }
        self.flash
            .get_mut()
            .get_mut()
            .read(self.layout.journal_start + offset, data)
            .map_err(flash_error)
    }
    pub fn journal_erase(&mut self, page: u32) -> Result<(), Error> {
        if self.selected || self.boot == State::Swap {
            return Err(Error::TrialPending);
        }
        let offset = page.checked_mul(PAGE_SIZE).ok_or(Error::Bounds)?;
        if offset
            .checked_add(PAGE_SIZE)
            .is_none_or(|end| end > self.journal_capacity())
        {
            return Err(Error::Bounds);
        }
        let start = self.layout.journal_start + offset;
        self.flash
            .get_mut()
            .get_mut()
            .erase(start, start + PAGE_SIZE)
            .map_err(flash_error)
    }
    pub fn journal_write(&mut self, offset: u32, data: &[u8]) -> Result<(), Error> {
        if self.selected || self.boot == State::Swap {
            return Err(Error::TrialPending);
        }
        if data.len() > 256
            || !offset.is_multiple_of(4)
            || !data.len().is_multiple_of(4)
            || offset
                .checked_add(data.len() as u32)
                .is_none_or(|end| end > self.journal_capacity())
        {
            return Err(Error::Bounds);
        }
        let mut aligned = AlignedBuffer([0; 256]);
        aligned.0[..data.len()].copy_from_slice(data);
        self.flash
            .get_mut()
            .get_mut()
            .write(self.layout.journal_start + offset, &aligned.0[..data.len()])
            .map_err(flash_error)
    }

    pub fn into_flash(self) -> F {
        self.flash.into_inner().into_inner()
    }
}

/// Run the shared swap/revert algorithm before starting the active image.
/// Only the loader calls this on hardware; it can overwrite the active slot.
/// Returns the resulting state, including Revert after restoring the prior image.
pub fn prepare_boot<F: NorFlash>(flash: F, layout: Layout) -> Result<(F, BootState), Error> {
    validate(&flash, layout)?;
    let flash = Mutex::new(RefCell::new(flash));
    let mut loader = BootLoader::new(BootLoaderConfig {
        active: partition(&flash, layout.active_start, layout.dfu_start),
        dfu: partition(&flash, layout.dfu_start, layout.journal_start),
        state: partition(&flash, layout.state_start, layout.active_start),
    });
    let mut page = AlignedBuffer([0; PAGE_SIZE as usize]);
    loader
        .prepare_boot(page.as_mut())
        .map_err(|_| Error::Boot)?;
    // Embassy returns the entry state even after reversion. Read the resulting
    // state so the loader arms trial supervision only for the actual trial.
    let state = loader.read_state(page.as_mut()).map_err(|_| Error::Boot)?;
    Ok((flash.into_inner().into_inner(), state))
}

#[cfg(test)]
mod tests;
