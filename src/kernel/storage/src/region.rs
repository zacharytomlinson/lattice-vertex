//! An exclusive, bounded view of a port-reserved flash region.

use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};

pub struct Region<'a, F: NorFlash> {
    flash: &'a mut F,
    start: u32,
    length: u32,
    writable: bool,
}
impl<'a, F: NorFlash> Region<'a, F> {
    pub fn new(flash: &'a mut F, start: u32, length: u32) -> Result<Self, NorFlashErrorKind> {
        if F::ERASE_SIZE == 0
            || F::ERASE_SIZE > u32::MAX as usize
            || length == 0
            || !start.is_multiple_of(F::ERASE_SIZE as u32)
            || !length.is_multiple_of(F::ERASE_SIZE as u32)
            || start
                .checked_add(length)
                .is_none_or(|end| end as usize > flash.capacity())
        {
            return Err(NorFlashErrorKind::OutOfBounds);
        }
        Ok(Self {
            flash,
            start,
            length,
            writable: true,
        })
    }
    pub fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }
    /// The port can temporarily prohibit mutation while an image is on trial.
    pub fn is_writable(&self) -> bool {
        self.writable
    }
    fn address(&self, offset: u32, length: usize) -> Result<u32, NorFlashErrorKind> {
        if length > u32::MAX as usize
            || offset
                .checked_add(length as u32)
                .is_none_or(|end| end > self.length)
        {
            return Err(NorFlashErrorKind::OutOfBounds);
        }
        Ok(self.start + offset)
    }
}
impl<F: NorFlash> ErrorType for Region<'_, F> {
    type Error = NorFlashErrorKind;
}
impl<F: NorFlash> ReadNorFlash for Region<'_, F> {
    const READ_SIZE: usize = F::READ_SIZE;
    fn capacity(&self) -> usize {
        self.length as usize
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        use embedded_storage::nor_flash::NorFlashError;
        let address = self.address(offset, bytes.len())?;
        self.flash.read(address, bytes).map_err(|e| e.kind())
    }
}
impl<F: NorFlash> NorFlash for Region<'_, F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        use embedded_storage::nor_flash::NorFlashError;
        if !self.writable {
            return Err(NorFlashErrorKind::Other);
        }
        let address = self.address(offset, bytes.len())?;
        self.flash.write(address, bytes).map_err(|e| e.kind())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        use embedded_storage::nor_flash::NorFlashError;
        if !self.writable {
            return Err(NorFlashErrorKind::Other);
        }
        let length = to.checked_sub(from).ok_or(NorFlashErrorKind::OutOfBounds)?;
        let address = self.address(from, length as usize)?;
        self.flash
            .erase(address, address + length)
            .map_err(|e| e.kind())
    }
}
