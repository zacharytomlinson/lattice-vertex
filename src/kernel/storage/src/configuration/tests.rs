extern crate std;
use super::*;
use embedded_storage::nor_flash::{ErrorType, NorFlashErrorKind, ReadNorFlash};

#[derive(Clone)]
struct Flash {
    bytes: [u8; 512],
    cut: Option<usize>,
    writes: usize,
}
impl Flash {
    fn new() -> Self {
        Self {
            bytes: [0xff; 512],
            cut: None,
            writes: 0,
        }
    }
    fn word(&mut self, at: usize, data: &[u8], erase: bool) -> Result<(), NorFlashErrorKind> {
        if self.cut == Some(0) {
            return Err(NorFlashErrorKind::Other);
        }
        for (dest, value) in self.bytes[at..at + 4].iter_mut().zip(data) {
            if !erase {
                assert_eq!(*dest & value, *value);
            }
            *dest = *value;
        }
        self.writes += 1;
        if let Some(n) = &mut self.cut {
            *n -= 1;
            if *n == 0 {
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
        self.bytes.len()
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let data = self
            .bytes
            .get(offset as usize..offset as usize + bytes.len())
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        bytes.copy_from_slice(data);
        Ok(())
    }
}
impl NorFlash for Flash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 64;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        assert_eq!(offset % 4, 0);
        assert_eq!(bytes.len() % 4, 0);
        for (i, data) in bytes.chunks(4).enumerate() {
            self.word(offset as usize + i * 4, data, false)?;
        }
        Ok(())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        assert_eq!(from % 64, 0);
        assert_eq!(to - from, 64);
        for at in (from as usize..to as usize).step_by(4) {
            self.word(at, &[0xff; 4], true)?;
        }
        Ok(())
    }
}
fn save(store: &mut Configuration, flash: &mut Flash, bytes: &[u8]) -> Result<Image, Error> {
    store.begin(bytes.len() as u32)?;
    while !store.erase_next_page(flash)? {}
    for (index, data) in bytes.chunks(256).enumerate() {
        store.write(flash, index as u32 * 256, data)?;
    }
    store.commit(flash)
}

#[test]
fn commit_atomicity() {
    // Cover the first replacement and reuse of a previously committed bank.
    for prior in [1, 2] {
        let mut baseline = Flash::new();
        let mut store = Configuration::open(&mut baseline).unwrap();
        for _ in 0..prior {
            save(&mut store, &mut baseline, b"old").unwrap();
        }
        let preserved = store.current().unwrap().bank as usize * 256;
        let mut complete = baseline.clone();
        let before = complete.writes;
        save(&mut store, &mut complete, b"new data").unwrap();
        for cut in 0..=complete.writes - before {
            let mut flash = baseline.clone();
            let mut store = Configuration::open(&mut flash).unwrap();
            flash.cut = Some(cut);
            let result = save(&mut store, &mut flash, b"new data");
            assert_eq!(
                &flash.bytes[preserved..preserved + 256],
                &baseline.bytes[preserved..preserved + 256]
            );
            flash.cut = None;
            let restored = Configuration::open(&mut flash).unwrap();
            let image = restored.current().unwrap();
            assert!(image.generation() == prior || image.generation() == prior + 1);
            let expected: &[u8] = if image.generation() == prior {
                b"old"
            } else {
                b"new data"
            };
            if result.is_err() && image.generation() == prior + 1 {
                assert_eq!(store.begin(0), Err(Error::Order));
            }
            let mut bytes = [0; 8];
            restored
                .read(&mut flash, 0, &mut bytes[..image.length() as usize])
                .unwrap();
            assert_eq!(&bytes[..image.length() as usize], expected);
        }
    }
}

#[test]
fn storage_bounds() {
    let mut flash = Flash::new();
    let mut store = Configuration::open(&mut flash).unwrap();
    let capacity = store.capacity();
    for length in [0, 1, capacity - 1, capacity, capacity + 1, u32::MAX] {
        assert_eq!(
            store.begin(length),
            if length <= capacity {
                Ok(())
            } else {
                Err(Error::Bounds)
            }
        );
    }
    assert_eq!(flash.writes, 0);
    store.begin(5).unwrap();
    assert_eq!(store.write(&mut flash, 0, b"abcd"), Err(Error::Incomplete));
    while !store.erase_next_page(&mut flash).unwrap() {}
    for (offset, bytes, expected) in [
        (4, &b"e"[..], Err(Error::Order)),
        (0, &b"abc"[..], Err(Error::Bounds)),
        (u32::MAX, &b"abcd"[..], Err(Error::Bounds)),
        (0, &b"abcd"[..], Ok(())),
        (0, &b"abcd"[..], Ok(())),
        (0, &b"xxxx"[..], Err(Error::Order)),
        (4, &b"e"[..], Ok(())),
    ] {
        assert_eq!(store.write(&mut flash, offset, bytes), expected);
    }
    store.commit(&mut flash).unwrap();
    assert_eq!(store.read(&mut flash, 5, &mut []), Ok(()));
    assert_eq!(store.read(&mut flash, 5, &mut [0]), Err(Error::Bounds));
    for length in [0, capacity as usize] {
        let bytes = std::vec![0x55; length];
        save(&mut store, &mut flash, &bytes).unwrap();
        store = Configuration::open(&mut flash).unwrap();
        let mut read = std::vec![0; length];
        store.read(&mut flash, 0, &mut read).unwrap();
        assert_eq!(read, bytes);
    }
    store.current.as_mut().unwrap().generation = u64::MAX;
    assert_eq!(store.begin(0), Err(Error::Order));
}

#[test]
fn committed_integrity() {
    let mut base = Flash::new();
    let mut store = Configuration::open(&mut base).unwrap();
    save(&mut store, &mut base, b"old").unwrap();
    save(&mut store, &mut base, b"new").unwrap();
    for at in [0, 4, 12, 16, 256 - 36, 256 - 1] {
        let mut flash = base.clone();
        flash.bytes[256 + at] ^= 1;
        let restored = Configuration::open(&mut flash).unwrap();
        assert_eq!(restored.current().unwrap().generation(), 1);
    }
    base.bytes[16] ^= 1;
    base.bytes[256 + 16] ^= 1;
    assert!(matches!(Configuration::open(&mut base), Err(Error::Digest)));
}

#[test]
fn region_bounds() {
    use crate::region::Region;
    let mut flash = Flash::new();
    {
        let mut region = Region::new(&mut flash, 64, 128).unwrap();
        for (offset, accepted) in [(0, true), (124, true), (128, false), (u32::MAX - 3, false)] {
            assert_eq!(region.write(offset, &[0; 4]).is_ok(), accepted);
        }
        for (from, to, accepted) in [
            (0, 64, true),
            (64, 128, true),
            (128, 192, false),
            (64, 0, false),
        ] {
            assert_eq!(region.erase(from, to).is_ok(), accepted);
        }
        let mut read_only = region.read_only();
        assert!(read_only.write(0, &[0; 4]).is_err());
        assert!(read_only.erase(0, 64).is_err());
        read_only.read(0, &mut [0; 4]).unwrap();
    }
    assert!(flash.bytes[..64]
        .iter()
        .chain(&flash.bytes[192..])
        .all(|b| *b == 0xff));
}
