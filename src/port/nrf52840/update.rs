//! Local Nordic image storage. Boot regions come from the selected board layout.
//! The common OTA service will own when these operations run. No radio endpoint
//! or appliance-facing update loop is installed by enabling this module.

use super::hal::{nvmc::Nvmc, peripherals::NVMC, Peri};
pub use lattice_nrf52840_boot::{layout, BootState, Error};
pub type ImageStorage<'a> = lattice_nrf52840_boot::ImageStorage<Nvmc<'a>>;

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
    ImageStorage::new(Nvmc::new(nvmc), layout)
}
