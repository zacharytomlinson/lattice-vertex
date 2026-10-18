extern crate std;
use self::std::{cell::RefCell, rc::Rc, vec, vec::Vec};
use super::*;
use ed25519_dalek::{Signer, SigningKey};
use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use sha2::{Digest, Sha256};

#[derive(Clone)]
struct Flash(Rc<RefCell<Memory>>);
struct Memory {
    bytes: Vec<u8>,
    writes: u32,
    selected: u32,
    torn_checkpoint: bool,
    image_read_bytes: usize,
}
impl Flash {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(Memory {
            bytes: vec![0xff; 16384],
            writes: 0,
            selected: 0,
            torn_checkpoint: false,
            image_read_bytes: 0,
        })))
    }
}
impl ErrorType for Flash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for Flash {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        16384
    }
    fn read(&mut self, o: u32, b: &mut [u8]) -> Result<(), Self::Error> {
        if o >= 8192 {
            self.0.borrow_mut().image_read_bytes += b.len();
        }
        b.copy_from_slice(
            self.0
                .borrow()
                .bytes
                .get(o as usize..o as usize + b.len())
                .ok_or(NorFlashErrorKind::OutOfBounds)?,
        );
        Ok(())
    }
}
impl NorFlash for Flash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;
    fn erase(&mut self, a: u32, b: u32) -> Result<(), Self::Error> {
        assert!(a.is_multiple_of(4096) && b == a + 4096 && b <= 16384);
        let mut m = self.0.borrow_mut();
        m.writes += 1;
        m.bytes[a as usize..b as usize].fill(0xff);
        Ok(())
    }
    fn write(&mut self, o: u32, b: &[u8]) -> Result<(), Self::Error> {
        assert!(o.is_multiple_of(4) && b.len().is_multiple_of(4));
        let mut m = self.0.borrow_mut();
        m.writes += 1;
        let cut = m.torn_checkpoint && o >= journal::RECORDS && o < 8192 && b.len() == 8;
        if cut {
            m.torn_checkpoint = false;
        }
        for (i, &v) in b[..if cut { 4 } else { b.len() }].iter().enumerate() {
            let old = &mut m.bytes[o as usize + i];
            assert_eq!(*old & v, v, "NOR overwrite");
            *old = v;
        }
        if cut {
            Err(NorFlashErrorKind::Other)
        } else {
            Ok(())
        }
    }
}
struct Backend {
    flash: Flash,
    candidate: Option<lattice_image_storage::Candidate<Flash>>,
}
impl Backend {
    fn new(flash: Flash) -> Self {
        Self {
            flash,
            candidate: None,
        }
    }
}
impl Storage for Backend {
    type Error = ();
    fn capacity(&self) -> u32 {
        8192
    }
    fn journal_capacity(&self) -> u32 {
        8192
    }
    fn journal_read(&mut self, o: u32, b: &mut [u8]) -> Result<(), ()> {
        assert!(o + b.len() as u32 <= 8192);
        self.flash.read(o, b).map_err(|_| ())
    }
    fn journal_erase(&mut self, p: u32) -> Result<(), ()> {
        assert!(p < 2);
        self.flash.erase(p * 4096, (p + 1) * 4096).map_err(|_| ())
    }
    fn journal_write(&mut self, o: u32, b: &[u8]) -> Result<(), ()> {
        assert!(o + b.len() as u32 <= 8192);
        self.flash.write(o, b).map_err(|_| ())
    }
    fn resume(&mut self, n: u32, o: u32) -> Result<(), ()> {
        let mut c = lattice_image_storage::Candidate::new(
            &self.flash,
            8192,
            8192,
            n,
            n.div_ceil(4096) * 4096,
        )
        .map_err(|_| ())?;
        c.resume(o).map_err(|_| ())?;
        self.candidate = Some(c);
        Ok(())
    }
    fn erase_next_page(&mut self) -> Result<bool, ()> {
        self.candidate
            .as_mut()
            .ok_or(())?
            .erase_next_page(&mut self.flash)
            .map_err(|_| ())
    }
    fn write(&mut self, o: u32, b: &[u8]) -> Result<(), ()> {
        self.candidate
            .as_mut()
            .ok_or(())?
            .write(&mut self.flash, o, b)
            .map_err(|_| ())
    }
    fn verify_step(&mut self, h: &[u8; 32], bytes: u32) -> Result<bool, ()> {
        self.candidate
            .as_mut()
            .ok_or(())?
            .verify_step(&mut self.flash, h, bytes)
            .map_err(|_| ())
    }
    fn select_trial(&mut self, h: &[u8; 32]) -> Result<(), ()> {
        self.candidate
            .as_mut()
            .ok_or(())?
            .verify(&mut self.flash, h)
            .map_err(|_| ())?;
        self.flash.0.borrow_mut().selected += 1;
        Ok(())
    }
    fn confirm(&mut self) -> Result<(), ()> {
        Ok(())
    }
}
fn policy() -> (Policy, SigningKey) {
    let key = SigningKey::from_bytes(&[7; 32]);
    (
        Policy {
            public_key: key.verifying_key().to_bytes(),
            platform: 1,
            layout: 2,
            appliance: [3; 16],
            surface: [4; 32],
            release: 1,
        },
        key,
    )
}
fn header(image: &[u8], p: &Policy, key: &SigningKey) -> [u8; HEADER_BYTES] {
    let mut b = [0; HEADER_BYTES];
    b[..4].copy_from_slice(b"LVUP");
    b[4] = 1;
    b[6..8].copy_from_slice(&p.platform.to_le_bytes());
    b[8..12].copy_from_slice(&p.layout.to_le_bytes());
    b[12..28].copy_from_slice(&p.appliance);
    b[28..60].copy_from_slice(&p.surface);
    b[60..68].copy_from_slice(&(p.release + 1).to_le_bytes());
    b[68..72].copy_from_slice(&(image.len() as u32).to_le_bytes());
    b[72..104].copy_from_slice(&Sha256::digest(image));
    b[104..128].fill(0xff);
    let signature = key.sign(&b[..128]);
    b[128..].copy_from_slice(&signature.to_bytes());
    b
}
fn offer(r: &mut Receiver<Backend>, b: &[u8; HEADER_BYTES]) -> Progress {
    let id = package::identity(b);
    let mut response = Progress::unsupported(id);
    for (i, data) in b.chunks(48).enumerate() {
        response = r.handle(Request::Header {
            package: id,
            offset: (i * 48) as u8,
            data,
        });
    }
    response
}
fn prepare(r: &mut Receiver<Backend>, id: PackageId) {
    for _ in 0..10 {
        let p = r.handle(Request::Prepare { package: id });
        assert_eq!(p.status, Status::Ok);
        if p.phase == Phase::Receiving {
            return;
        }
    }
    panic!("preparation did not finish");
}

#[test]
fn package_authorization_precedes_all_flash_mutation() {
    let (policy, key) = policy();
    let image = [0x55; 64];
    for failure in 0..5 {
        let mut b = header(&image, &policy, &key);
        let expected = match failure {
            0 => {
                b[104] ^= 1;
                Status::Signature
            }
            1 => {
                b[8] ^= 1;
                Status::Target
            }
            2 => {
                b[28] ^= 1;
                Status::Target
            }
            3 => {
                b[60..68].copy_from_slice(&1u64.to_le_bytes());
                Status::Version
            }
            _ => {
                b[68..72].copy_from_slice(&9000u32.to_le_bytes());
                Status::Target
            }
        };
        if failure != 0 {
            let sig = key.sign(&b[..128]);
            b[128..].copy_from_slice(&sig.to_bytes());
        }
        let flash = Flash::new();
        let mut r = Receiver::new(Backend::new(flash.clone()), policy);
        assert_eq!(offer(&mut r, &b).status, expected);
        assert_eq!(flash.0.borrow().writes, 0);
    }
}

#[test]
fn authenticated_header_keeps_its_buffer_until_journal_commit() {
    let (policy, key) = policy();
    let first = header(&[1; 64], &policy, &key);
    let other = header(&[2; 64], &policy, &key);
    let id = package::identity(&first);
    let flash = Flash::new();
    let mut r = Receiver::new(Backend::new(flash.clone()), policy);
    assert_eq!(offer(&mut r, &first).status, Status::Ok);
    assert_eq!(offer(&mut r, &other).status, Status::Busy);
    prepare(&mut r, id);
    drop(r);
    let r = Receiver::new(Backend::new(flash), policy);
    assert_eq!(r.header.unwrap().id, id);
}

#[test]
fn local_cost_estimates_preserve_unknown_and_overflow() {
    use package::Costs;
    let c = Costs {
        erase_page_us: 100,
        write_per_byte: 128,
        verify_per_byte: 64,
        activation_us: 10,
        swap_per_byte: 128,
        longest_pause_us: 100,
    };
    assert_eq!(c.staging_us(4, 2), Some(203));
    assert_eq!(c.activation_us(4), Some(12));
    assert_eq!(
        Costs {
            write_per_byte: u32::MAX,
            ..c
        }
        .staging_us(4, 2),
        None
    );
    assert_eq!(
        Costs {
            erase_page_us: u32::MAX - 1,
            write_per_byte: u32::MAX - 1,
            verify_per_byte: u32::MAX - 1,
            ..c
        }
        .staging_us(u32::MAX, u32::MAX),
        None
    );
}

#[test]
fn acknowledged_bytes_and_durable_pages_survive_restart_and_duplicate_delivery() {
    let (policy, key) = policy();
    let image = vec![0x55; 4168];
    let b = header(&image, &policy, &key);
    let id = package::identity(&b);
    let flash = Flash::new();
    let mut r = Receiver::new(Backend::new(flash.clone()), policy);
    assert_eq!(offer(&mut r, &b).status, Status::Ok);
    prepare(&mut r, id);
    for offset in (0..4128).step_by(48) {
        assert_eq!(
            r.handle(Request::Data {
                package: id,
                offset: offset as u32,
                data: &image[offset..offset + 48]
            })
            .status,
            Status::Ok
        );
    }
    let p = r.handle(Request::Status { package: id });
    assert_eq!((p.accepted, p.committed), (4128, 4096));
    let writes = flash.0.borrow().writes;
    assert_eq!(
        r.handle(Request::Data {
            package: id,
            offset: 4080,
            data: &image[4080..4128]
        })
        .status,
        Status::Ok
    );
    assert_eq!(flash.0.borrow().writes, writes);
    drop(r);
    let mut r = Receiver::new(Backend::new(flash.clone()), policy);
    let p = r.handle(Request::Status { package: id });
    assert_eq!((p.accepted, p.committed), (4096, 4096));
    prepare(&mut r, id);
    for offset in (4096..image.len()).step_by(48) {
        let end = (offset + 48).min(image.len());
        assert_eq!(
            r.handle(Request::Data {
                package: id,
                offset: offset as u32,
                data: &image[offset..end]
            })
            .status,
            Status::Ok
        );
    }
    let read_before = flash.0.borrow().image_read_bytes;
    assert_eq!(
        r.handle(Request::Finish { package: id }).phase,
        Phase::Verifying
    );
    assert_eq!(flash.0.borrow().image_read_bytes - read_before, 256);
    assert_eq!(
        r.activate(),
        Err(Status::Order),
        "partial digest never selects a trial"
    );
    // A reset during verification retains the image, but restarts its RAM hash.
    drop(r);
    let mut r = Receiver::new(Backend::new(flash.clone()), policy);
    let read_before = flash.0.borrow().image_read_bytes;
    for step in 0..image.len().div_ceil(256) {
        let before = flash.0.borrow().image_read_bytes;
        let p = r.handle(Request::Finish { package: id });
        assert_eq!(p.status, Status::Ok);
        assert!(flash.0.borrow().image_read_bytes - before <= 256);
        assert_eq!(
            p.phase,
            if step + 1 == image.len().div_ceil(256) {
                Phase::Ready
            } else {
                Phase::Verifying
            }
        );
    }
    assert_eq!(flash.0.borrow().image_read_bytes - read_before, image.len());
    assert_eq!(flash.0.borrow().selected, 0, "receiving never activates");
    let verified = flash.0.borrow().image_read_bytes;
    r.activate().unwrap();
    assert_eq!(
        flash.0.borrow().image_read_bytes,
        verified,
        "selection must not repeat the full scan"
    );
    assert_eq!(flash.0.borrow().selected, 1);
    drop(r);
    let mut r = Receiver::new(Backend::new(flash), policy);
    assert_eq!(
        r.handle(Request::Status { package: id }).phase,
        Phase::Idle,
        "consumed journal cannot resurrect a trial"
    );
}

#[test]
fn torn_checkpoint_is_skipped_and_bad_image_is_never_selected() {
    let (policy, key) = policy();
    let image = vec![0x55; 72];
    let b = header(&image, &policy, &key);
    let id = package::identity(&b);
    let flash = Flash::new();
    let mut r = Receiver::new(Backend::new(flash.clone()), policy);
    offer(&mut r, &b);
    prepare(&mut r, id);
    r.handle(Request::Data {
        package: id,
        offset: 0,
        data: &image[..48],
    });
    flash.0.borrow_mut().torn_checkpoint = true;
    assert_eq!(
        r.handle(Request::Data {
            package: id,
            offset: 48,
            data: &image[48..]
        })
        .status,
        Status::Storage
    );
    drop(r);
    let mut r = Receiver::new(Backend::new(flash.clone()), policy);
    assert_eq!(r.committed, 0);
    assert_eq!(r.next_record, journal::RECORDS + 8);
    prepare(&mut r, id);
    r.handle(Request::Data {
        package: id,
        offset: 0,
        data: &[0x44; 48],
    });
    r.handle(Request::Data {
        package: id,
        offset: 48,
        data: &image[48..],
    });
    assert_eq!(
        r.handle(Request::Finish { package: id }).status,
        Status::Verification
    );
    assert!(r.activate().is_err());
    assert_eq!(flash.0.borrow().selected, 0);
    drop(r);
    let r = Receiver::new(Backend::new(flash), policy);
    assert!(r.header.is_none());
}
