// SPDX-License-Identifier: MIT
//! C6 flash/boot integration. Uses the installed partition table and actual MMU
//! mapping, never a caller's guess about which OTA image is running.

use esp_bootloader_esp_idf::partitions::{
    self, AppPartitionSubType as App, DataPartitionSubType as Data, PartitionType as Kind,
};
use esp_storage::FlashStorage;
pub use lattice_c6_boot::{BootState, Error, Layout, Region};
pub type ImageStorage<'a> = lattice_c6_boot::ImageStorage<FlashStorage<'a>>;

pub fn image_storage(flash: esp_hal::peripherals::FLASH<'_>) -> Result<ImageStorage<'_>, Error> {
    // This first backend writes plaintext application and OTA records only.
    if esp_hal::efuse::flash_encryption() {
        return Err(Error::Layout);
    }
    let mut flash = FlashStorage::new(flash);
    let mut bytes = [0; partitions::PARTITION_TABLE_MAX_LEN];
    let table =
        partitions::read_partition_table(&mut flash, &mut bytes).map_err(|_| Error::Layout)?;
    let booted = table
        .booted_partition()
        .map_err(|_| Error::RunningSlot)?
        .ok_or(Error::RunningSlot)?;
    let running = match booted.partition_type() {
        Kind::App(App::Ota0) => 0,
        Kind::App(App::Ota1) => 1,
        _ => return Err(Error::RunningSlot),
    };
    let mut images = [None; 2];
    let mut ota_data = None;
    let mut journal = None;
    for (i, entry) in table.iter().enumerate() {
        let end = entry
            .offset()
            .checked_add(entry.len())
            .ok_or(Error::Layout)?;
        for prior in table.iter().take(i) {
            let prior_end = prior
                .offset()
                .checked_add(prior.len())
                .ok_or(Error::Layout)?;
            if entry.offset() < prior_end && prior.offset() < end {
                return Err(Error::Layout);
            }
        }
        let destination = if entry.label_as_str() == "lattice_update" {
            if entry.raw_type() != 1 || entry.raw_subtype() != 0x40 {
                return Err(Error::Layout);
            }
            &mut journal
        } else {
            match entry.partition_type() {
                Kind::App(App::Ota0) => &mut images[0],
                Kind::App(App::Ota1) => &mut images[1],
                Kind::App(_) => return Err(Error::Layout), // exactly two OTA slots
                Kind::Data(Data::Ota) => &mut ota_data,
                _ => continue,
            }
        };
        if destination.is_some() || entry.flags() != 0 {
            return Err(Error::Layout);
        }
        *destination = Some(Region {
            offset: entry.offset(),
            length: entry.len(),
        });
    }
    let layout = Layout {
        journal,
        ota_data: ota_data.ok_or(Error::Layout)?,
        images: [
            images[0].ok_or(Error::Layout)?,
            images[1].ok_or(Error::Layout)?,
        ],
    };
    ImageStorage::new(flash, layout, running)
}
