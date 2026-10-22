//! Two committed copies of local definitions. No firmware image or work replay.
//! The port supplies a protected Region; only the inactive copy is mutated.
//! Calls run through the kernel's flash owner, never from an ISR.

use super::{flash_error, Buffer, Error};
use embedded_storage::nor_flash::NorFlash;
use sha2::{Digest, Sha256};

const HEADER: u32 = 16;
const FOOTER: u32 = 36; // SHA-256 followed by the final commit word.
const MAGIC: [u8; 4] = *b"LCF\x01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    bank: u32,
    generation: u64,
    length: u32,
}
impl Image {
    pub fn generation(self) -> u64 {
        self.generation
    }
    pub fn length(self) -> u32 {
        self.length
    }
}
struct Pending {
    image: Image,
    erased: u32,
    written: u32,
    header_written: bool,
    failed: bool,
    hash: Sha256,
}
pub struct Configuration {
    bank_bytes: u32,
    erase_bytes: usize,
    uncertain: bool,
    current: Option<Image>,
    pending: Option<Pending>,
}
fn header(image: Image) -> [u8; HEADER as usize] {
    let mut bytes = [0; HEADER as usize];
    bytes[..4].copy_from_slice(&MAGIC);
    bytes[4..12].copy_from_slice(&image.generation.to_le_bytes());
    bytes[12..].copy_from_slice(&image.length.to_le_bytes());
    bytes
}
fn digest<F: NorFlash>(flash: &mut F, start: u32, length: u32) -> Result<[u8; 32], Error> {
    let mut hash = Sha256::new();
    let mut bytes = Buffer([0; 256]);
    let mut at = 0;
    while at < length {
        let n = (length - at).min(256) as usize;
        flash
            .read(start + at, &mut bytes.0[..n])
            .map_err(flash_error)?;
        hash.update(&bytes.0[..n]);
        at += n as u32;
    }
    Ok(hash.finalize().into())
}
impl Configuration {
    pub fn open<F: NorFlash>(flash: &mut F) -> Result<Self, Error> {
        let capacity = flash.capacity();
        if F::READ_SIZE != 1
            || F::WRITE_SIZE != 4
            || F::ERASE_SIZE < 64
            || !F::ERASE_SIZE.is_multiple_of(4)
            || capacity > u32::MAX as usize
            || capacity == 0
            || !capacity.is_multiple_of(F::ERASE_SIZE.checked_mul(2).ok_or(Error::Bounds)?)
        {
            return Err(Error::Bounds);
        }
        let bank_bytes = capacity as u32 / 2;
        let mut this = Self {
            bank_bytes,
            erase_bytes: F::ERASE_SIZE,
            uncertain: false,
            current: None,
            pending: None,
        };
        let mut invalid_committed = false;
        for bank in 0..2 {
            let start = bank * bank_bytes;
            let mut h = [0; HEADER as usize];
            let mut footer = [0; FOOTER as usize];
            flash.read(start, &mut h).map_err(flash_error)?;
            flash
                .read(start + bank_bytes - FOOTER, &mut footer)
                .map_err(flash_error)?;
            if footer[32..] != [0; 4] {
                continue;
            }
            if h[..4] != MAGIC {
                invalid_committed = true;
                continue;
            }
            let generation = u64::from_le_bytes(h[4..12].try_into().unwrap());
            let length = u32::from_le_bytes(h[12..16].try_into().unwrap());
            if generation == 0
                || length > this.capacity()
                || digest(flash, start, HEADER + length)? != footer[..32]
            {
                invalid_committed = true;
                continue;
            }
            if this.current.is_some_and(|i| i.generation == generation) {
                return Err(Error::Order);
            }
            if this.current.is_none_or(|i| generation > i.generation) {
                this.current = Some(Image {
                    bank,
                    generation,
                    length,
                });
            }
        }
        if this.current.is_none() && invalid_committed {
            return Err(Error::Digest);
        }
        Ok(this)
    }
    pub fn capacity(&self) -> u32 {
        self.bank_bytes - HEADER - FOOTER
    }
    pub fn current(&self) -> Option<Image> {
        self.current
    }
    fn check<F: NorFlash>(&self, flash: &F) -> Result<(), Error> {
        if F::READ_SIZE != 1
            || F::WRITE_SIZE != 4
            || F::ERASE_SIZE != self.erase_bytes
            || flash.capacity() != (2 * self.bank_bytes) as usize
        {
            return Err(Error::Bounds);
        }
        Ok(())
    }
    pub fn read<F: NorFlash>(
        &self,
        flash: &mut F,
        offset: u32,
        bytes: &mut [u8],
    ) -> Result<(), Error> {
        self.check(flash)?;
        let image = self.current.ok_or(Error::Incomplete)?;
        if bytes.len() > u32::MAX as usize
            || offset
                .checked_add(bytes.len() as u32)
                .is_none_or(|end| end > image.length)
        {
            return Err(Error::Bounds);
        }
        flash
            .read(image.bank * self.bank_bytes + HEADER + offset, bytes)
            .map_err(flash_error)
    }
    /// Begin a replacement, including an empty configuration. No old bytes change.
    pub fn begin(&mut self, length: u32) -> Result<(), Error> {
        if self.uncertain {
            return Err(Error::Order);
        }
        if length > self.capacity() {
            return Err(Error::Bounds);
        }
        let image = Image {
            bank: self.current.map_or(0, |i| 1 - i.bank),
            generation: self
                .current
                .map_or(Some(1), |i| i.generation.checked_add(1))
                .ok_or(Error::Order)?,
            length,
        };
        let mut hash = Sha256::new();
        hash.update(header(image));
        self.pending = Some(Pending {
            image,
            erased: 0,
            written: 0,
            header_written: false,
            failed: false,
            hash,
        });
        Ok(())
    }
    /// One erase page per call, so installation can yield between flash pauses.
    pub fn erase_next_page<F: NorFlash>(&mut self, flash: &mut F) -> Result<bool, Error> {
        self.check(flash)?;
        let pending = self.pending.as_mut().ok_or(Error::Order)?;
        if pending.failed || pending.header_written {
            return Err(Error::Order);
        }
        if pending.erased < self.bank_bytes {
            let start = pending.image.bank * self.bank_bytes + pending.erased;
            flash
                .erase(start, start + F::ERASE_SIZE as u32)
                .map_err(flash_error)?;
            pending.erased += F::ERASE_SIZE as u32;
        }
        Ok(pending.erased == self.bank_bytes)
    }
    /// Sequential words, at most 256 bytes. Only the final chunk may be unaligned.
    /// An identical duplicate is accepted without another NOR write.
    pub fn write<F: NorFlash>(
        &mut self,
        flash: &mut F,
        offset: u32,
        bytes: &[u8],
    ) -> Result<(), Error> {
        self.check(flash)?;
        let pending = self.pending.as_mut().ok_or(Error::Order)?;
        if pending.failed {
            return Err(Error::Order);
        }
        if pending.erased != self.bank_bytes {
            return Err(Error::Incomplete);
        }
        let end = offset
            .checked_add(bytes.len().try_into().map_err(|_| Error::Bounds)?)
            .ok_or(Error::Bounds)?;
        if bytes.is_empty()
            || bytes.len() > 256
            || !offset.is_multiple_of(4)
            || end > pending.image.length
            || (end != pending.image.length && !bytes.len().is_multiple_of(4))
        {
            return Err(Error::Bounds);
        }
        let start = pending.image.bank * self.bank_bytes;
        let mut buffer = Buffer([0xff; 256]);
        if end <= pending.written {
            flash
                .read(start + HEADER + offset, &mut buffer.0[..bytes.len()])
                .map_err(flash_error)?;
            return if &buffer.0[..bytes.len()] == bytes {
                Ok(())
            } else {
                Err(Error::Order)
            };
        }
        if offset != pending.written {
            return Err(Error::Order);
        }
        pending.failed = true; // Any uncertain write requires a fresh erase.
        if !pending.header_written {
            flash
                .write(start, &header(pending.image))
                .map_err(flash_error)?;
            pending.header_written = true;
        }
        buffer.0[..bytes.len()].copy_from_slice(bytes);
        flash
            .write(
                start + HEADER + offset,
                &buffer.0[..bytes.len().div_ceil(4) * 4],
            )
            .map_err(flash_error)?;
        pending.hash.update(bytes);
        pending.written = end;
        pending.failed = false;
        Ok(())
    }
    /// Verify physical bytes before publishing the final commit word. A failed
    /// call may have committed; reopen to determine the durable result, not replay.
    pub fn commit<F: NorFlash>(&mut self, flash: &mut F) -> Result<Image, Error> {
        self.check(flash)?;
        let pending = self.pending.as_mut().ok_or(Error::Order)?;
        if pending.failed {
            return Err(Error::Order);
        }
        if pending.erased != self.bank_bytes || pending.written != pending.image.length {
            return Err(Error::Incomplete);
        }
        pending.failed = true;
        let start = pending.image.bank * self.bank_bytes;
        if !pending.header_written {
            flash
                .write(start, &header(pending.image))
                .map_err(flash_error)?;
        }
        let expected: [u8; 32] = pending.hash.clone().finalize().into();
        if digest(flash, start, HEADER + pending.image.length)? != expected {
            return Err(Error::Digest);
        }
        let footer = start + self.bank_bytes - FOOTER;
        flash.write(footer, &expected).map_err(flash_error)?;
        // Do not claim completion if the flash driver returned success but failed
        // to retain the footer or commit word.
        let mut check = [0; FOOTER as usize];
        flash.read(footer, &mut check).map_err(flash_error)?;
        if check[..32] != expected || check[32..] != [0xff; 4] {
            return Err(Error::Digest);
        }
        self.uncertain = true;
        flash.write(footer + 32, &[0; 4]).map_err(flash_error)?;
        flash
            .read(footer + 32, &mut check[..4])
            .map_err(flash_error)?;
        if check[..4] != [0; 4] {
            return Err(Error::Digest);
        }
        let image = pending.image;
        self.current = Some(image);
        self.uncertain = false;
        self.pending = None;
        Ok(image)
    }
}

#[cfg(test)]
mod tests;
