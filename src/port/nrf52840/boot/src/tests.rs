extern crate std;

use super::*;
use embedded_storage::nor_flash::{ErrorType, ReadNorFlash};
use sha2::{Digest, Sha256};
use std::{rc::Rc, vec, vec::Vec};

const LAYOUT: Layout = Layout {
    loader_start: 0x1000,
    state_start: 0x2000,
    active_start: 0x3000,
    dfu_start: 0x6000,
    journal_start: 0xa000,
    end: 0xc000,
};

#[derive(Clone)]
struct Flash(Rc<RefCell<Memory>>);
struct Memory {
    bytes: Vec<u8>,
    writes: Vec<u32>,
    cut_after: Option<usize>,
}
impl Flash {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(Memory {
            bytes: vec![0xff; 0x10000],
            writes: vec![],
            cut_after: None,
        })))
    }
    fn cut(memory: &mut Memory) -> Result<(), NorFlashErrorKind> {
        if let Some(n) = memory.cut_after.as_mut() {
            *n -= 1;
            if *n == 0 {
                memory.cut_after = None;
                return Err(NorFlashErrorKind::Other);
            }
        }
        Ok(())
    }
    fn active(&self) -> Vec<u8> {
        self.0.borrow().bytes[LAYOUT.active_start as usize..LAYOUT.dfu_start as usize].to_vec()
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
        let m = self.0.borrow();
        let source = m
            .bytes
            .get(offset as usize..offset as usize + data.len())
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        data.copy_from_slice(source);
        Ok(())
    }
}
impl NorFlash for Flash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        assert_eq!(offset % 4, 0);
        assert_eq!(data.len() % 4, 0);
        assert!((LAYOUT.state_start..LAYOUT.end).contains(&offset));
        assert!(offset as usize + data.len() <= LAYOUT.end as usize);
        let mut m = self.0.borrow_mut();
        for (i, &byte) in data.iter().enumerate() {
            let old = &mut m.bytes[offset as usize + i];
            assert_eq!(*old & byte, byte, "NOR 0 -> 1 without erase");
            *old = byte;
        }
        m.writes.push(offset);
        Self::cut(&mut m)
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        assert_eq!(from % PAGE_SIZE, 0);
        assert_eq!(to % PAGE_SIZE, 0);
        assert!(from >= LAYOUT.state_start && from < to && to <= LAYOUT.end);
        let mut m = self.0.borrow_mut();
        m.bytes[from as usize..to as usize].fill(0xff);
        Self::cut(&mut m)
    }
}

fn image(byte: u8) -> Vec<u8> {
    let mut bytes = vec![byte; 521]; // final partial word and chunk
    bytes[..4].copy_from_slice(&layout::RAM_END.to_le_bytes());
    bytes[4..8].copy_from_slice(&(LAYOUT.active_start + 9).to_le_bytes());
    bytes
}
fn stage(flash: Flash, bytes: &[u8]) -> ImageStorage<Flash> {
    let mut storage = ImageStorage::new(flash, LAYOUT).unwrap();
    storage.begin(bytes.len() as u32).unwrap();
    while !storage.erase_next_page().unwrap() {}
    for (i, chunk) in bytes.chunks(256).enumerate() {
        storage.write((i * 256) as u32, chunk).unwrap();
    }
    storage
}
fn original(flash: &Flash) -> Vec<u8> {
    let mut bytes = vec![0xff; LAYOUT.image_capacity() as usize];
    let original = image(0x55);
    bytes[..original.len()].copy_from_slice(&original);
    flash.0.borrow_mut().bytes[LAYOUT.active_start as usize..LAYOUT.dfu_start as usize]
        .copy_from_slice(&bytes);
    bytes
}

#[test]
fn layout_and_staging_boundaries() {
    assert!(layout::XIAO.valid());
    assert_eq!(layout::XIAO.image_capacity(), 372 * 1024);
    for (valid, crc, length, allowed) in [
        (1u16, 0x1234u16, 2804u32, true),
        (1, 0x1234, 0x8000, true),
        (1, 0x1234, 0x8004, false),
        (1, 0x1234, 0, false),
        (0xffff, 0, 0, false),
        (1, 0, 0x66000, true),
    ] {
        let mut settings = [0; 12];
        settings[..2].copy_from_slice(&valid.to_le_bytes());
        settings[2..4].copy_from_slice(&crc.to_le_bytes());
        settings[8..].copy_from_slice(&length.to_le_bytes());
        assert_eq!(layout::xiao_factory_crc_allows_updates(&settings), allowed);
    }
    for bad in [
        Layout {
            dfu_start: LAYOUT.active_start,
            ..LAYOUT
        },
        Layout {
            end: LAYOUT.end - 1,
            ..LAYOUT
        },
        Layout {
            journal_start: LAYOUT.dfu_start + LAYOUT.image_capacity(),
            ..LAYOUT
        },
    ] {
        assert!(!bad.valid());
    }
    let flash = Flash::new();
    let old = original(&flash);
    let bytes = image(0xa5);
    let mut storage = ImageStorage::new(flash.clone(), LAYOUT).unwrap();
    assert_eq!(storage.begin(u32::MAX), Err(Error::Bounds));
    storage.begin(bytes.len() as u32).unwrap();
    assert_eq!(storage.write(0, &bytes[..4]), Err(Error::Incomplete));
    while !storage.erase_next_page().unwrap() {}
    assert_eq!(storage.write(u32::MAX - 3, &bytes[..8]), Err(Error::Bounds));
    assert_eq!(storage.write(0, &bytes[..3]), Err(Error::Bounds));
    assert_eq!(storage.write(4, &bytes[..4]), Err(Error::Order));
    for (i, chunk) in bytes.chunks(256).enumerate() {
        storage.write((i * 256) as u32, chunk).unwrap();
    }
    let writes = flash.0.borrow().writes.len();
    storage.write(0, &bytes[..256]).unwrap();
    storage.write(512, &bytes[512..]).unwrap();
    assert_eq!(writes, flash.0.borrow().writes.len());
    assert_eq!(storage.write(8, &[0; 4]), Err(Error::Order));
    assert_eq!(storage.select_trial(&[0; 32]), Err(Error::Digest));
    assert_eq!(flash.active(), old);
    assert!(
        flash.0.borrow().bytes[LAYOUT.dfu_start as usize + bytes.len()..LAYOUT.end as usize]
            .iter()
            .all(|&b| b == 0xff)
    );
}

#[test]
fn trial_confirmation_and_reversion() {
    for confirm in [false, true] {
        let flash = Flash::new();
        let old = original(&flash);
        let bytes = image(0xa5);
        let mut storage = stage(flash.clone(), &bytes);
        storage
            .select_trial(&Sha256::digest(&bytes).into())
            .unwrap();
        assert_eq!(storage.confirm(), Err(Error::TrialPending));
        assert_eq!(storage.begin(128), Err(Error::TrialPending));
        drop(storage);
        let (_, state) = prepare_boot(flash.clone(), LAYOUT).unwrap();
        assert_eq!(state, State::Swap);
        assert_eq!(&flash.active()[..bytes.len()], &bytes);
        let mut trial = ImageStorage::new(flash.clone(), LAYOUT).unwrap();
        assert_eq!(trial.begin(128), Err(Error::TrialPending));
        if confirm {
            trial.confirm().unwrap();
        }
        drop(trial);
        let (_, state) = prepare_boot(flash.clone(), LAYOUT).unwrap();
        assert_eq!(state, if confirm { State::Boot } else { State::Revert });
        if confirm {
            assert_eq!(&flash.active()[..bytes.len()], &bytes);
        } else {
            assert_eq!(flash.active(), old);
        }
    }
}

#[test]
fn interrupted_staging_and_swaps_preserve_recovery() {
    for cut in [1, 2, 5, 11] {
        let flash = Flash::new();
        let old = original(&flash);
        let bytes = image(0xa5);
        let mut staging = stage(flash.clone(), &bytes);
        staging
            .select_trial(&Sha256::digest(&bytes).into())
            .unwrap();
        drop(staging);
        flash.0.borrow_mut().cut_after = Some(cut);
        assert!(prepare_boot(flash.clone(), LAYOUT).is_err());
        prepare_boot(flash.clone(), LAYOUT).unwrap();
        assert_eq!(&flash.active()[..bytes.len()], &bytes);
        flash.0.borrow_mut().cut_after = Some(cut);
        assert!(prepare_boot(flash.clone(), LAYOUT).is_err());
        prepare_boot(flash.clone(), LAYOUT).unwrap();
        assert_eq!(flash.active(), old);
    }
    let flash = Flash::new();
    let old = original(&flash);
    let mut storage = ImageStorage::new(flash.clone(), LAYOUT).unwrap();
    storage.begin(128).unwrap();
    while !storage.erase_next_page().unwrap() {}
    flash.0.borrow_mut().cut_after = Some(1);
    assert!(storage.write(0, &[0; 128]).is_err());
    assert_eq!(storage.write(0, &[0; 128]), Err(Error::Order));
    drop(storage);
    prepare_boot(flash.clone(), LAYOUT).unwrap();
    assert_eq!(flash.active(), old);
}

#[test]
fn image_must_be_complete_and_linked_for_active_slot() {
    for vectors in [[0; 8], [0xff; 8], {
        let mut v = [0; 8];
        v[..4].copy_from_slice(&layout::RAM_END.to_le_bytes());
        v[4..].copy_from_slice(&(LAYOUT.dfu_start + 9).to_le_bytes());
        v
    }] {
        let mut bytes = image(0xa5);
        bytes[..8].copy_from_slice(&vectors);
        let mut storage = stage(Flash::new(), &bytes);
        assert_eq!(
            storage.select_trial(&Sha256::digest(&bytes).into()),
            Err(Error::Vectors)
        );
    }
    let mut storage = ImageStorage::new(Flash::new(), LAYOUT).unwrap();
    storage.begin(128).unwrap();
    assert_eq!(storage.select_trial(&[0; 32]), Err(Error::Incomplete));
}
