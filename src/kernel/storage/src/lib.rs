#![no_std]
// SPDX-License-Identifier: MIT
//! Bounded flash regions, image staging and committed local configuration.
//! Ports own the physical device, reservations and boot policy. This crate owns
//! byte acceptance, erase progress, integrity and publication of complete copies.
//! No network handler, allocator or appliance code lives here.

use embedded_storage::nor_flash::{NorFlash, NorFlashError, NorFlashErrorKind};
use sha2::{Digest, Sha256};

pub mod configuration;
pub mod region;

#[repr(align(4))]
struct Buffer([u8; 256]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Order,
    Incomplete,
    Digest,
    Flash(NorFlashErrorKind),
}
fn flash_error(error: impl NorFlashError) -> Error {
    Error::Flash(error.kind())
}

/// One image in a port-reserved inactive region. `erase_length` can cover the
/// whole slot for a swapping loader or only the image's sectors for direct boot.
pub struct Candidate<F: NorFlash> {
    _flash: core::marker::PhantomData<fn() -> F>,
    destination: u32,
    length: u32,
    erase_length: u32,
    erased: u32,
    written: u32,
    failed: bool,
    verification: Verification,
}

enum Verification {
    NotStarted,
    Reading { hash: Sha256, offset: u32 },
    Complete([u8; 32]),
}

impl<F: NorFlash> Candidate<F> {
    pub fn new(
        flash: &F,
        destination: u32,
        capacity: u32,
        length: u32,
        erase_length: u32,
    ) -> Result<Self, Error> {
        if F::WRITE_SIZE != 4
            || F::READ_SIZE != 1
            || F::ERASE_SIZE == 0
            || F::ERASE_SIZE > u32::MAX as usize
            || !F::ERASE_SIZE.is_multiple_of(4)
            || !destination.is_multiple_of(F::ERASE_SIZE as u32)
            || !capacity.is_multiple_of(F::ERASE_SIZE as u32)
            || length == 0
            || length > capacity
            || erase_length < length
            || erase_length > capacity
            || !erase_length.is_multiple_of(F::ERASE_SIZE as u32)
            || destination
                .checked_add(capacity)
                .is_none_or(|end| end as usize > flash.capacity())
        {
            return Err(Error::Bounds);
        }
        Ok(Self {
            _flash: core::marker::PhantomData,
            destination,
            length,
            erase_length,
            erased: 0,
            written: 0,
            failed: false,
            verification: Verification::NotStarted,
        })
    }

    pub fn length(&self) -> u32 {
        self.length
    }

    /// Resume only at a durable page boundary (or a complete image). Erase the
    /// unfinished page and remainder again before accepting further writes.
    pub fn resume(&mut self, completed: u32) -> Result<(), Error> {
        if completed > self.length
            || (completed != self.length && !completed.is_multiple_of(F::ERASE_SIZE as u32))
        {
            return Err(Error::Bounds);
        }
        self.written = completed;
        self.erased = if completed == self.length {
            self.erase_length
        } else {
            completed
        };
        self.failed = false;
        self.verification = Verification::NotStarted;
        Ok(())
    }

    pub fn erase_next_page(&mut self, flash: &mut F) -> Result<bool, Error> {
        if self.failed {
            return Err(Error::Order);
        }
        if self.erased < self.erase_length {
            self.verification = Verification::NotStarted;
            let start = self.destination + self.erased;
            flash
                .erase(start, start + F::ERASE_SIZE as u32)
                .map_err(flash_error)?;
            self.erased += F::ERASE_SIZE as u32;
        }
        Ok(self.erased == self.erase_length)
    }

    pub fn write(&mut self, flash: &mut F, offset: u32, bytes: &[u8]) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Order);
        }
        let end = offset
            .checked_add(bytes.len().try_into().map_err(|_| Error::Bounds)?)
            .ok_or(Error::Bounds)?;
        if bytes.is_empty()
            || bytes.len() > 256
            || !offset.is_multiple_of(4)
            || end > self.length
            || (end != self.length && !bytes.len().is_multiple_of(4))
        {
            return Err(Error::Bounds);
        }
        if self.erased != self.erase_length {
            return Err(Error::Incomplete);
        }
        let mut buffer = Buffer([0xff; 256]);
        if end <= self.written {
            flash
                .read(self.destination + offset, &mut buffer.0[..bytes.len()])
                .map_err(flash_error)?;
            return if &buffer.0[..bytes.len()] == bytes {
                Ok(())
            } else {
                Err(Error::Order)
            };
        }
        if offset != self.written {
            return Err(Error::Order);
        }
        buffer.0[..bytes.len()].copy_from_slice(bytes);
        self.verification = Verification::NotStarted;
        if let Err(error) = flash.write(
            self.destination + offset,
            &buffer.0[..bytes.len().div_ceil(4) * 4],
        ) {
            // An uncertain NOR write must be erased before another write attempt.
            self.failed = true;
            return Err(flash_error(error));
        }
        self.written = end;
        Ok(())
    }

    /// Verify at most `bytes` image bytes, using the same fixed scratch buffer.
    /// The completed digest remains valid while this candidate exclusively owns
    /// the unchanged region. Resume, erase, and new writes invalidate it.
    pub fn verify_step(
        &mut self,
        flash: &mut F,
        expected_sha256: &[u8; 32],
        bytes: u32,
    ) -> Result<bool, Error> {
        if self.failed {
            return Err(Error::Order);
        }
        if self.written != self.length {
            return Err(Error::Incomplete);
        }
        if bytes == 0 {
            return Err(Error::Bounds);
        }
        if let Verification::Complete(digest) = &self.verification {
            return if digest == expected_sha256 {
                Ok(true)
            } else {
                Err(Error::Digest)
            };
        }
        if matches!(self.verification, Verification::NotStarted) {
            self.verification = Verification::Reading {
                hash: Sha256::new(),
                offset: 0,
            };
        }
        let Verification::Reading { hash, offset } = &mut self.verification else {
            unreachable!()
        };
        let end = self.length.min(offset.saturating_add(bytes));
        let mut buffer = Buffer([0; 256]);
        while *offset < end {
            let len = core::cmp::min(buffer.0.len(), (end - *offset) as usize);
            flash
                .read(self.destination + *offset, &mut buffer.0[..len])
                .map_err(flash_error)?;
            hash.update(&buffer.0[..len]);
            *offset += len as u32;
        }
        if *offset != self.length {
            return Ok(false);
        }
        let Verification::Reading { hash, .. } =
            core::mem::replace(&mut self.verification, Verification::NotStarted)
        else {
            unreachable!()
        };
        let digest: [u8; 32] = hash.finalize().into();
        self.verification = Verification::Complete(digest);
        if &digest != expected_sha256 {
            return Err(Error::Digest);
        }
        Ok(true)
    }

    /// Synchronous convenience for local port users. Kernel staging calls
    /// verify_step; selection of an already verified image only checks the cache.
    pub fn verify(&mut self, flash: &mut F, expected_sha256: &[u8; 32]) -> Result<(), Error> {
        while !self.verify_step(flash, expected_sha256, 1024)? {}
        Ok(())
    }
}
