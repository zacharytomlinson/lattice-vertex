extern crate std;
use super::*;
use embedded_storage::nor_flash::{ErrorType, ReadNorFlash};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

const LAYOUT: Layout = Layout {
    journal: None,
    configuration: Some(Region {
        offset: 0x32000,
        length: 0x2000,
    }),
    ota_data: Region {
        offset: 0xe000,
        length: 0x2000,
    },
    images: [
        Region {
            offset: 0x10000,
            length: 0x10000,
        },
        Region {
            offset: 0x20000,
            length: 0x10000,
        },
    ],
};

#[derive(Clone)]
struct Flash(Rc<RefCell<Memory>>);
struct Memory {
    bytes: Vec<u8>,
    writes: usize,
    cut: Option<usize>,
}
impl Flash {
    fn new(records: [Option<Record>; 2]) -> Self {
        let mut bytes = vec![0xff; 0x40000];
        for (index, record) in records.iter().enumerate() {
            if let Some(record) = record {
                let start = LAYOUT.ota_data.offset as usize + index * SECTOR_SIZE as usize;
                bytes[start..start + 32].copy_from_slice(&record.encode());
            }
        }
        bytes[0x10000..0x20000].fill(0x55); // running-image sentinel
        bytes[0x32000..0x34000].fill(0x5a); // configuration sentinel
        Self(Rc::new(RefCell::new(Memory {
            bytes,
            writes: 0,
            cut: None,
        })))
    }
    fn after(m: &mut Memory) -> Result<(), NorFlashErrorKind> {
        m.writes += 1;
        if let Some(left) = m.cut.as_mut() {
            *left -= 1;
            if *left == 0 {
                m.cut = None;
                return Err(NorFlashErrorKind::Other);
            }
        }
        Ok(())
    }
}
impl ErrorType for Flash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for Flash {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.0.borrow().bytes.len()
    }
    fn read(&mut self, offset: u32, data: &mut [u8]) -> Result<(), Self::Error> {
        data.copy_from_slice(
            self.0
                .borrow()
                .bytes
                .get(offset as usize..offset as usize + data.len())
                .ok_or(NorFlashErrorKind::OutOfBounds)?,
        );
        Ok(())
    }
}
impl NorFlash for Flash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        assert_eq!(offset % 4, 0);
        assert_eq!(data.len() % 4, 0);
        let mut m = self.0.borrow_mut();
        for (i, &byte) in data.iter().enumerate() {
            let dest = &mut m.bytes[offset as usize + i];
            assert_eq!(*dest & byte, byte, "NOR overwrite without erase");
            *dest = byte;
        }
        Self::after(&mut m)
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        assert_eq!(from % SECTOR_SIZE, 0);
        assert_eq!(to - from, SECTOR_SIZE);
        let mut m = self.0.borrow_mut();
        m.bytes[from as usize..to as usize].fill(0xff);
        Self::after(&mut m)
    }
}
fn valid(sequence: u32) -> Option<Record> {
    Some(Record {
        sequence,
        state: BootState::Valid,
    })
}
fn image() -> Vec<u8> {
    let mut bytes = vec![0x55; 521];
    bytes[..24].fill(0);
    bytes[0] = 0xe9;
    bytes[1] = 1;
    bytes[12] = 13;
    bytes
}
fn stage(flash: Flash, bytes: &[u8]) -> ImageStorage<Flash> {
    let mut storage = ImageStorage::new(flash, LAYOUT, 0).unwrap();
    storage.begin(bytes.len() as u32).unwrap();
    while !storage.erase_next_page().unwrap() {}
    for (i, chunk) in bytes.chunks(256).enumerate() {
        storage.write(i as u32 * 256, chunk).unwrap();
    }
    storage
}

#[test]
fn metadata_matches_reference_and_ignores_rejected_or_torn_records() {
    // Golden CRCs from esp-bootloader-esp-idf's ESP-IDF-format record fixtures.
    for (sequence, crc) in [(1, 0x4743989a), (2, 0x55f63774), (3, 0xed4a5011)] {
        assert_eq!(metadata::sequence_crc(sequence), crc);
    }
    for state in [BootState::Invalid, BootState::Aborted] {
        let records = [valid(1), Some(Record { sequence: 2, state })];
        let flash = Flash::new(records);
        let mut storage = stage(flash.clone(), &image());
        storage
            .select_trial(&Sha256::digest(image()).into())
            .unwrap();
        let new = metadata::read(&mut flash.clone(), LAYOUT.ota_data.offset).unwrap();
        assert_eq!(new[0], valid(1));
        assert_eq!(
            new[1],
            Some(Record {
                sequence: 4,
                state: BootState::New
            })
        );
        assert_eq!(storage.begin(128), Err(Error::TrialPending));
        assert_eq!(storage.confirm(), Err(Error::TrialPending));
    }
    let raw = valid(2).unwrap().encode();
    for index in [0, 24, 28] {
        let mut torn = raw;
        torn[index] ^= 0x80;
        assert!(Record::decode(&torn).is_none());
    }
    assert!(Record::decode(&[0xff; 32]).is_none());
    assert_eq!(
        metadata::next_sequence(&[valid(u32::MAX - 1), None], 0),
        Err(Error::SequenceExhausted)
    );
}

#[test]
fn pending_trial_cannot_destroy_rollback_and_confirmation_updates_only_running_record() {
    let flash = Flash::new([
        valid(1),
        Some(Record {
            sequence: 2,
            state: BootState::PendingVerify,
        }),
    ]);
    let before = flash.0.borrow().bytes.clone();
    assert!(matches!(
        ImageStorage::new(flash.clone(), LAYOUT, 0),
        Err(Error::RunningSlot)
    ));
    let mut storage = ImageStorage::new(flash.clone(), LAYOUT, 1).unwrap();
    assert_eq!(storage.begin(521), Err(Error::TrialPending));
    let mut config = storage.configuration().unwrap();
    assert!(!config.is_writable());
    let mut retained = [0; 4];
    config.read(0, &mut retained).unwrap();
    assert_eq!(retained, [0x5a; 4]);
    assert!(config.erase(0, SECTOR_SIZE).is_err());
    assert_eq!(flash.0.borrow().bytes, before);
    storage.confirm().unwrap();
    assert_eq!(storage.boot_state(), BootState::Valid);
    assert!(storage.configuration().unwrap().is_writable());
    assert_eq!(
        metadata::read(&mut flash.clone(), LAYOUT.ota_data.offset).unwrap(),
        [valid(1), valid(2)]
    );
    assert_eq!(&flash.0.borrow().bytes[0x10000..], &before[0x10000..]);
    let flash = Flash::new([
        valid(1),
        Some(Record {
            sequence: 2,
            state: BootState::New,
        }),
    ]);
    assert!(matches!(
        ImageStorage::new(flash, LAYOUT, 1),
        Err(Error::RollbackDisabled)
    ));
}

#[test]
fn interrupted_metadata_commit_preserves_confirmed_record() {
    for cut in [1, 2, 3] {
        let flash = Flash::new([valid(1), None]);
        let mut storage = stage(flash.clone(), &image());
        flash.0.borrow_mut().cut = Some(cut);
        assert!(storage
            .select_trial(&Sha256::digest(image()).into())
            .is_err());
        assert_eq!(storage.begin(521), Err(Error::TrialPending));
        let records = metadata::read(&mut flash.clone(), LAYOUT.ota_data.offset).unwrap();
        assert_eq!(records[0], valid(1));
        let selected = metadata::selected(&records).unwrap();
        assert_eq!(selected, if cut == 3 { 1 } else { 0 });
        assert!(flash.0.borrow().bytes[0x10000..0x20000]
            .iter()
            .all(|&b| b == 0x55));
    }
}

#[test]
fn staging_bounds_duplicates_and_image_validation() {
    let flash = Flash::new([valid(1), None]);
    let bytes = image();
    let mut storage = ImageStorage::new(flash.clone(), LAYOUT, 0).unwrap();
    assert_eq!(storage.begin(u32::MAX), Err(Error::Bounds));
    storage.begin(bytes.len() as u32).unwrap();
    assert_eq!(storage.write(0, &bytes[..4]), Err(Error::Incomplete));
    assert_eq!(storage.select_trial(&[0; 32]), Err(Error::Incomplete));
    assert!(storage.erase_next_page().unwrap());
    assert_eq!(storage.write(u32::MAX - 3, &bytes[..8]), Err(Error::Bounds));
    assert_eq!(storage.write(4, &bytes[..4]), Err(Error::Order));
    for (i, chunk) in bytes.chunks(256).enumerate() {
        storage.write(i as u32 * 256, chunk).unwrap();
    }
    let writes = flash.0.borrow().writes;
    storage.write(0, &bytes[..256]).unwrap();
    storage.write(512, &bytes[512..]).unwrap();
    assert_eq!(flash.0.borrow().writes, writes);
    assert_eq!(storage.select_trial(&[0; 32]), Err(Error::Digest));
    assert_eq!(
        metadata::read(&mut flash.clone(), LAYOUT.ota_data.offset).unwrap(),
        [valid(1), None]
    );
    assert!(flash.0.borrow().bytes[0x10000..0x20000]
        .iter()
        .all(|&b| b == 0x55));
    assert!(flash.0.borrow().bytes[0x32000..0x34000]
        .iter()
        .all(|b| *b == 0x5a));
    for index in [0, 1, 12, 23] {
        let mut bad = image();
        bad[index] = 0xff;
        let mut storage = stage(Flash::new([valid(1), None]), &bad);
        assert_eq!(
            storage.select_trial(&Sha256::digest(&bad).into()),
            Err(Error::Image)
        );
    }
    let overlapping = Layout {
        images: [LAYOUT.images[0]; 2],
        ..LAYOUT
    };
    assert!(matches!(
        ImageStorage::new(Flash::new([valid(1), None]), overlapping, 0),
        Err(Error::Layout)
    ));
    for (offset, length) in [
        (0xe000, 0x2000),
        (0x10000, 0x2000),
        (0x20000, 0x2000),
        (0x31000, 0x1000),
        (0x3f000, 0x2000),
    ] {
        let bad = Layout {
            configuration: Some(Region { offset, length }),
            ..LAYOUT
        };
        assert!(!bad.valid(0x40000));
    }
}

#[test]
fn configuration_region() {
    let flash = Flash::new([valid(1), None]);
    let before = flash.0.borrow().bytes.clone();
    let mut storage = ImageStorage::new(flash.clone(), LAYOUT, 0).unwrap();
    let mut region = storage.configuration().unwrap();
    let mut config = lattice_storage::configuration::Configuration::open(&mut region).unwrap();
    config.begin(4).unwrap();
    while !config.erase_next_page(&mut region).unwrap() {}
    config.write(&mut region, 0, b"data").unwrap();
    config.commit(&mut region).unwrap();
    drop(storage);
    let mut storage = ImageStorage::new(flash.clone(), LAYOUT, 0).unwrap();
    let mut region = storage.configuration().unwrap();
    let config = lattice_storage::configuration::Configuration::open(&mut region).unwrap();
    let mut bytes = [0; 4];
    config.read(&mut region, 0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"data");
    assert_eq!(&flash.0.borrow().bytes[..0x32000], &before[..0x32000]);
    assert_eq!(&flash.0.borrow().bytes[0x34000..], &before[0x34000..]);
}
