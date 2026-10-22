extern crate std;
use super::*;
use crate::{
    ir::Type,
    kernel::peer::frame::{InvokeStatus, StateReadResult},
    sync::TaskContext,
};
use core::sync::atomic::AtomicU16;
use embedded_storage::nor_flash::{ErrorType, NorFlashErrorKind, ReadNorFlash};
use std::{vec, vec::Vec};

const STATE: u64 = 0x4000000000000001;
const CALL: u64 = 0x8000000000000002;
const SURFACE: [u8; 32] = [7; 32];
struct Device {
    head: AtomicU16,
}
impl Device {
    fn new() -> Self {
        Self {
            head: AtomicU16::new(NONE),
        }
    }
}
impl Native for Device {
    fn ready(&self) -> bool {
        true
    }
    fn owns(&self, _: *const (), _: u64) -> bool {
        false
    }
    fn read(&self, _: &TaskContext, _: u64) -> StateReadResult {
        panic!("loading must not sample hardware")
    }
    fn invoke(&self, _: &TaskContext, _: u64, _: &[crate::ir::Value]) -> InvokeStatus {
        panic!("loading must not mutate hardware")
    }
    fn state_binding(&self, r: u64) -> Option<(Type, &AtomicU16)> {
        (r == STATE).then_some((Type::Bool, &self.head))
    }
    fn transformer_signature(&self, r: u64) -> Option<&[Type]> {
        (r == CALL).then_some(&[Type::Bool])
    }
    fn clear_dependencies(&self) {
        self.head.store(NONE, Ordering::Release);
    }
}
struct Flash(Vec<u8>);
impl Flash {
    fn new() -> Self {
        Self(vec![0xff; 16_384])
    }
}
impl ErrorType for Flash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for Flash {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.0.len()
    }
    fn read(&mut self, at: u32, b: &mut [u8]) -> Result<(), Self::Error> {
        b.copy_from_slice(&self.0[at as usize..at as usize + b.len()]);
        Ok(())
    }
}
impl NorFlash for Flash {
    const ERASE_SIZE: usize = 4096;
    const WRITE_SIZE: usize = 4;
    fn write(&mut self, at: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        assert_eq!(at % 4, 0);
        assert_eq!(bytes.len() % 4, 0);
        for (old, new) in self.0[at as usize..at as usize + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            assert_eq!(*old & new, *new);
            *old = *new;
        }
        Ok(())
    }
    fn erase(&mut self, start: u32, end: u32) -> Result<(), Self::Error> {
        assert_eq!(end - start, 4096);
        self.0[start as usize..end as usize].fill(0xff);
        Ok(())
    }
}
// Encoded declaration: entry -> read Bool -> branch -> native call(constant).
// This is input data for the loader, not a firmware evaluator or a native call.
fn definition(argument: bool) -> Vec<u8> {
    let mut b = vec![0; 60];
    b[..4].copy_from_slice(b"LPD\x01");
    b[6..8].copy_from_slice(&1u16.to_le_bytes());
    b[8..10].copy_from_slice(&1u16.to_le_bytes());
    b[10..12].copy_from_slice(&60u16.to_le_bytes());
    b[12..14].copy_from_slice(&60u16.to_le_bytes());
    b[16..48].copy_from_slice(&SURFACE);
    b[48..56].copy_from_slice(&STATE.to_le_bytes());
    b[56..58].copy_from_slice(&60u16.to_le_bytes());
    b[58..60].fill(0xff);
    b.extend_from_slice(&[8, 0, 0, 0, 0, 1, 68, 0]);
    b.extend_from_slice(&[18, 0, 1]);
    b.extend_from_slice(&STATE.to_le_bytes());
    b.extend_from_slice(&[2, 0, 0, 0, 1, 86, 0]);
    b.extend_from_slice(&[11, 0, 5, 2, 0, 0, 0, 1, 97, 0, 0]);
    b.extend_from_slice(&[17, 0, 6]);
    b.extend_from_slice(&CALL.to_le_bytes());
    b.extend_from_slice(&[1, 0, 0, argument as u8, 0, 0]);
    b
}

#[test]
fn record_admission() {
    let native = Device::new();
    let good = definition(true);
    assert_eq!(validation::validate(&good, &native, &SURFACE), Ok(()));
    for (offset, value, error) in [
        (3, 2, Error::Encoding),
        (16, 8, Error::Compatibility),
        (48, 3, Error::Binding),
        (71, 3, Error::Binding),
        (65, 1, Error::Link),   // local target repaired below to a backward link
        (81, 1, Error::Bounds), // scratch destination beyond its one byte
        (111, 2, Error::Encoding), // non-Boolean literal
        (100, 3, Error::Binding),
    ] {
        let mut b = good.clone();
        b[offset] = value;
        if offset == 65 {
            b[66..68].copy_from_slice(&60u16.to_le_bytes());
        }
        assert_eq!(
            validation::validate(&b, &native, &SURFACE),
            Err(error),
            "offset {offset}"
        );
    }
    for end in 0..good.len() {
        assert!(
            validation::validate(&good[..end], &native, &SURFACE).is_err(),
            "prefix {end}"
        );
    }
    // Skipping the only writer cannot manufacture an initialized Boolean.
    let mut bypass = good.clone();
    bypass[66..68].copy_from_slice(&86u16.to_le_bytes());
    assert!(validation::validate(&bypass, &native, &SURFACE).is_err());
    assert_eq!(native.head.load(Ordering::Acquire), NONE);
    // External action entries need carried arguments, but no upstream activation
    // edge: the transformer itself is never an observed dependency.
    let mut terminal = vec![0; 48];
    terminal[..4].copy_from_slice(b"LPD\x01");
    terminal[4..6].copy_from_slice(&1u16.to_le_bytes());
    terminal[10..12].fill(0xff);
    terminal[12..14].copy_from_slice(&48u16.to_le_bytes());
    terminal[16..48].copy_from_slice(&SURFACE);
    terminal.extend_from_slice(&[12, 0, 0, 0, 1, 1, 0, 0, 0, 1, 60, 0]);
    terminal.extend_from_slice(&[18, 0, 6]);
    terminal.extend_from_slice(&CALL.to_le_bytes());
    terminal.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0]);
    assert_eq!(validation::validate(&terminal, &native, &SURFACE), Ok(()));
    // A join may consume a location only when both branches initialized it.
    for (false_target, expected) in [(99u16, Ok(())), (117, Err(Error::Type))] {
        let mut b = good[..86].to_vec();
        b[6..8].copy_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&[13, 0, 5, 2, 0, 0, 0, 1, 99, 0, 1]);
        b.extend_from_slice(&false_target.to_le_bytes());
        b.extend_from_slice(&[18, 0, 1]);
        b.extend_from_slice(&STATE.to_le_bytes());
        b.extend_from_slice(&[2, 0, 1, 0, 1, 117, 0]);
        b.extend_from_slice(&[18, 0, 6]);
        b.extend_from_slice(&CALL.to_le_bytes());
        b.extend_from_slice(&[1, 2, 0, 1, 0, 0, 0]);
        assert_eq!(validation::validate(&b, &native, &SURFACE), expected);
    }
    // A source cannot route upstream to a source-free action entry with no head.
    let mut dead = good[..60].to_vec();
    dead[10..12].fill(0xff);
    dead[56..58].copy_from_slice(&66u16.to_le_bytes());
    dead.extend_from_slice(&[6, 0, 0, 0, 0, 0]);
    dead.extend_from_slice(&[8, 0, 0, 1, 60, 0, 0, 0]);
    assert_eq!(
        validation::validate(&dead, &native, &SURFACE),
        Err(Error::Link)
    );
}

#[test]
fn installation_publication() {
    let native = Device::new();
    let mut flash = Flash::new();
    let mut programs = Programs::new();
    programs.restore(&mut flash, &native, &SURFACE).unwrap();
    let mut references = Vec::new();
    for (transfer, argument) in [(1, true), (2, false)] {
        let bytes = definition(argument);
        let digest = Sha256::digest(&bytes).into();
        let before = native.head.load(Ordering::Acquire);
        let reply = programs.receive(
            5,
            Request::Begin {
                transfer,
                length: bytes.len() as u16,
            },
            &native,
            &SURFACE,
        );
        assert_eq!(reply.status, Status::Receiving);
        let reference = (reply.offset, reply.generation);
        assert_eq!(
            programs
                .receive(
                    6,
                    Request::Begin {
                        transfer: 9,
                        length: 114
                    },
                    &native,
                    &SURFACE
                )
                .status,
            Status::Busy
        );
        for (i, chunk) in bytes.chunks(DATA_BYTES).enumerate() {
            let request = Request::Data {
                transfer,
                offset: (i * DATA_BYTES) as u16,
                bytes: chunk,
            };
            for _ in 0..2 {
                assert_eq!(
                    programs.receive(5, request, &native, &SURFACE).status,
                    Status::Receiving
                );
            }
        }
        let commit = Request::Commit { transfer, digest };
        assert_eq!(
            programs.receive(5, commit, &native, &SURFACE).status,
            Status::Persisting
        );
        assert_eq!(native.head.load(Ordering::Acquire), before);
        assert_eq!(
            programs
                .receive(
                    5,
                    Request::Commit {
                        transfer,
                        digest: [0; 32]
                    },
                    &native,
                    &SURFACE
                )
                .status,
            Status::Conflict
        );
        while programs.pending_write() {
            programs.poll(&mut flash, &native).unwrap();
        }
        let reply = programs.receive(5, commit, &native, &SURFACE);
        assert_eq!(
            (reply.status, reply.offset, reply.generation),
            (Status::Installed, reference.0, reference.1)
        );
        references.push(reference);
    }
    // Reconstruct against new native storage, using only committed bytes.
    let restarted = Device::new();
    let mut restored = Programs::new();
    restored.restore(&mut flash, &restarted, &SURFACE).unwrap();
    let mut link = restarted.head.load(Ordering::Acquire);
    for expected in references.iter().rev() {
        let (definition, generation, source, next) = restored.dependency(link).unwrap();
        assert_eq!((definition, generation), *expected);
        assert_eq!((source.state, source.entry), (STATE, 60));
        link = next;
    }
    assert_eq!(link, NONE);
    let read = restored.receive(
        5,
        Request::Read {
            offset: references[1].0 + PREFIX as u16 + 48,
        },
        &restarted,
        &SURFACE,
    );
    assert_eq!(&read.bytes[10..12], &[0xff; 2]); // runtime sibling words never escape
    assert_eq!(
        restored
            .receive(
                5,
                Request::Commit {
                    transfer: 1,
                    digest: [0; 32]
                },
                &restarted,
                &SURFACE
            )
            .status,
        Status::Conflict
    );
    let old = flash.0.clone();
    assert_eq!(
        restored.restore(&mut flash, &restarted, &[8; 32]),
        Err(Status::Compatibility)
    );
    assert_eq!(restarted.head.load(Ordering::Acquire), NONE);
    assert_eq!(flash.0, old);
}

#[test]
fn partial_installation() {
    for accepted in [0, 1, 48, 113, 114] {
        let native = Device::new();
        let mut flash = Flash::new();
        let mut programs = Programs::new();
        programs.restore(&mut flash, &native, &SURFACE).unwrap();
        let bytes = definition(true);
        programs.receive(
            5,
            Request::Begin {
                transfer: 1,
                length: 114,
            },
            &native,
            &SURFACE,
        );
        for (i, data) in bytes[..accepted].chunks(DATA_BYTES).enumerate() {
            programs.receive(
                5,
                Request::Data {
                    transfer: 1,
                    offset: (i * DATA_BYTES) as u16,
                    bytes: data,
                },
                &native,
                &SURFACE,
            );
        }
        if accepted != 114 {
            assert_eq!(
                programs
                    .receive(
                        5,
                        Request::Commit {
                            transfer: 1,
                            digest: Sha256::digest(&bytes).into()
                        },
                        &native,
                        &SURFACE
                    )
                    .status,
                Status::Invalid
            );
        }
        let mut restarted = Programs::new();
        restarted.restore(&mut flash, &native, &SURFACE).unwrap();
        assert_eq!(restarted.used, 4);
        assert_eq!(native.head.load(Ordering::Acquire), NONE);
    }
}

#[test]
fn reservation_expiry() {
    for started in [0, u32::MAX - 10_000] {
        let native = Device::new();
        let mut flash = Flash::new();
        let mut p = Programs::new();
        p.restore(&mut flash, &native, &SURFACE).unwrap();
        p.receive(
            5,
            Request::Begin {
                transfer: 1,
                length: 114,
            },
            &native,
            &SURFACE,
        );
        p.last_activity = started;
        p.expire(started.wrapping_add(29_999));
        assert!(p.pending.is_some());
        p.expire(started.wrapping_add(30_000));
        assert!(p.pending.is_none());
        assert_eq!(native.head.load(Ordering::Acquire), NONE);
        assert!(p.store.as_ref().unwrap().current().is_none());
    }
}
