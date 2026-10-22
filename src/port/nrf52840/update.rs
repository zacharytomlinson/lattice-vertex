//! Local Nordic image storage. Boot regions come from the selected board layout.
//! The common OTA service will own when these operations run. No radio endpoint
//! or appliance-facing update loop is installed by enabling this module.

use super::hal::{nvmc::Nvmc, peripherals::NVMC, Peri};
pub use lattice_nrf52840_boot::{layout, BootState, Error};
pub type ImageStorage<'a> = lattice_nrf52840_boot::ImageStorage<Flash<'a>>;

/// Bind the flash peripheral only when executing the layout's active image.
/// The bootloader and application must have been provisioned with the same layout.
pub fn image_storage(
    nvmc: Peri<'_, NVMC>,
    layout: layout::Layout,
) -> Result<ImageStorage<'_>, Error> {
    let vectors = unsafe { cortex_m::Peripherals::steal().SCB.vtor.read() };
    if !layout.valid() || vectors != layout.active_start {
        return Err(Error::Layout);
    }
    if layout == self::layout::XIAO {
        let descriptor = unsafe { &*(self::layout::DESCRIPTOR_ADDRESS as *const [u32; 16]) };
        if *descriptor != self::layout::DESCRIPTOR {
            return Err(Error::Layout);
        }
        // Adafruit/Seeed settings: bank validity, CRC16, padding, image length.
        // Refuse writes while factory CRC still covers mutable application data.
        let settings = unsafe { &*(0xff000 as *const [u8; 12]) };
        if !self::layout::xiao_factory_crc_allows_updates(settings) {
            return Err(Error::FactoryCrc);
        }
    }
    ImageStorage::new(Flash(Nvmc::new(nvmc)), layout)
}

// NVMC stalls instruction fetch. SysTick retains only one pending interrupt;
// preserve the other elapsed periods for civil/solar time, without changing RTOS
// scheduling or taking a TIMER/RTC away from the appliance. DWT is used only
// while the CPU is awake in a synchronous flash operation (< one 67-second wrap).
use cortex_m::peripheral::{DWT, SCB, SYST};
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
static mut MISSED_TICKS: u64 = 0;
/// Physical flash with elapsed-time accounting around blocking NVMC operations.
pub struct Flash<'a>(Nvmc<'a>);
/// Caller masks interrupts so the correction and RTOS count form one sample.
pub(crate) fn elapsed_ticks() -> u64 {
    (unsafe { MISSED_TICKS }) + u64::from(SCB::is_pendst_pending())
}
fn timed<R>(f: impl FnOnce() -> R) -> R {
    cortex_m::interrupt::free(|_| {
        let period = SYST::get_reload() + 1;
        let pending = SCB::is_pendst_pending();
        let started = DWT::cycle_count();
        let remaining = SYST::get_current();
        let result = f();
        let cycles = DWT::cycle_count().wrapping_sub(started) as u64;
        let wraps = (cycles + (period - 1 - remaining) as u64) / period as u64;
        let retained = u64::from(!pending && SCB::is_pendst_pending());
        unsafe {
            MISSED_TICKS += wraps.saturating_sub(retained);
        }
        result
    })
}
impl ErrorType for Flash<'_> {
    type Error = <Nvmc<'static> as ErrorType>::Error;
}
impl ReadNorFlash for Flash<'_> {
    const READ_SIZE: usize = Nvmc::READ_SIZE;
    fn capacity(&self) -> usize {
        self.0.capacity()
    }
    fn read(&mut self, at: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(at, bytes)
    }
}
impl NorFlash for Flash<'_> {
    const WRITE_SIZE: usize = Nvmc::WRITE_SIZE;
    const ERASE_SIZE: usize = Nvmc::ERASE_SIZE;
    fn write(&mut self, at: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        timed(|| self.0.write(at, bytes))
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        timed(|| self.0.erase(from, to))
    }
}
