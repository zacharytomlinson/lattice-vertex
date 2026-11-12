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
    fn read(&self, _: u64) -> StateReadResult {
        panic!("loading must not sample hardware")
    }
    fn invoke(&self, _: u64, _: &[crate::ir::Value]) -> InvokeStatus {
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
    let mut programs = Configuration::new();
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
        for (i, chunk) in bytes.chunks(DATA_BYTES).enumerate().rev() {
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
    let mut restored = Configuration::new();
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
    assert!(restored.configuration_ready());
    assert!(restored.recovery(0).is_none());
    restored.checkpoint_settings(&restarted).unwrap();
    persist(&mut restored, &mut flash, &restarted);
    assert_eq!(restored.state, Status::Compatibility);
    assert_eq!(restarted.head.load(Ordering::Acquire), NONE);
    // A compatible image can recover the retained definitions after a settings commit.
    restored.restore(&mut flash, &restarted, &SURFACE).unwrap();
    assert_ne!(restarted.head.load(Ordering::Acquire), NONE);
}

#[test]
fn incoming_fragment_bounds() {
    let native = Device::new();
    for (length, accepted) in [
        (0, false),
        (47, false),
        (48, true),
        (511, true),
        (512, true),
        (513, false),
        (8192, false),
        (u16::MAX, false),
    ] {
        let mut flash = Flash::new();
        let mut p = Configuration::new();
        p.restore(&mut flash, &native, &SURFACE).unwrap();
        let info = p.receive(5, Request::Info, &native, &SURFACE);
        assert_eq!(
            u16::from_le_bytes(info.bytes[4..6].try_into().unwrap()),
            512
        );
        assert!(u16::from_le_bytes(info.bytes[..2].try_into().unwrap()) > 512);
        assert_eq!(
            p.receive(
                5,
                Request::Begin {
                    transfer: 1,
                    length
                },
                &native,
                &SURFACE
            )
            .status,
            if accepted {
                Status::Receiving
            } else {
                Status::Bounds
            }
        );
        assert_eq!(p.pending.is_some(), accepted);
        if accepted {
            let bytes = [0; DATA_BYTES];
            for offset in (0..length as usize).step_by(DATA_BYTES) {
                let end = (length as usize - offset).min(DATA_BYTES);
                assert_eq!(
                    p.receive(
                        5,
                        Request::Data {
                            transfer: 1,
                            offset: offset as u16,
                            bytes: &bytes[..end]
                        },
                        &native,
                        &SURFACE
                    )
                    .status,
                    Status::Receiving
                );
            }
            assert_eq!(
                p.receive(
                    5,
                    Request::Data {
                        transfer: 1,
                        offset: length,
                        bytes: &[0]
                    },
                    &native,
                    &SURFACE
                )
                .status,
                Status::Bounds
            );
            assert_eq!(p.pending.unwrap().accepted, length);
        }
        assert!(p.store.as_ref().unwrap().current().is_none());
        assert_eq!(native.head.load(Ordering::Acquire), NONE);
    }
}

#[test]
fn partial_installation() {
    for accepted in [0, 1, 48, 113, 114] {
        let native = Device::new();
        let mut flash = Flash::new();
        let mut programs = Configuration::new();
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
        let mut restarted = Configuration::new();
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
        let mut p = Configuration::new();
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

fn stage(p: &mut Configuration, bytes: &[u8], transfer: u32, native: &dyn Native) -> (u16, u32) {
    let r = p.receive(
        5,
        Request::Begin {
            transfer,
            length: bytes.len() as u16,
        },
        native,
        &SURFACE,
    );
    assert_eq!(r.status, Status::Receiving);
    let identity = (r.offset, r.generation);
    for (i, b) in bytes.chunks(DATA_BYTES).enumerate() {
        assert_eq!(
            p.receive(
                5,
                Request::Data {
                    transfer,
                    offset: (i * DATA_BYTES) as u16,
                    bytes: b
                },
                native,
                &SURFACE
            )
            .status,
            Status::Receiving
        );
    }
    assert_eq!(
        p.receive(
            5,
            Request::Commit {
                transfer,
                digest: Sha256::digest(bytes).into()
            },
            native,
            &SURFACE
        )
        .status,
        Status::Persisting
    );
    identity
}
fn persist(p: &mut Configuration, flash: &mut Flash, native: &dyn Native) {
    while p.pending_write() {
        p.poll(flash, native).unwrap();
    }
}
fn install(bytes: &[u8], native: &dyn Native, flash: &mut Flash) -> Configuration {
    let mut p = Configuration::new();
    p.restore(flash, native, &SURFACE).unwrap();
    stage(&mut p, bytes, 1, native);
    persist(&mut p, flash, native);
    p
}
#[test]
fn admission_commit() {
    use lattice_peer_wire::program::execution::Request as E;
    for steps in 0..=5 {
        let native = Device::new();
        let mut flash = Flash::new();
        let mut p = install(&definition(true), &native, &mut flash);
        let enable = E::Enable {
            definition: 4,
            generation: 1,
            revision: 0,
            enabled: true,
        };
        assert_eq!(p.admission(enable).status, Status::Persisting);
        assert!(!p.enabled(4));
        assert!(p.initial().is_none());
        for _ in 0..steps {
            p.poll(&mut flash, &native).unwrap();
        }
        let committed = p.enabled(4);
        let mut restored = Configuration::new();
        restored.restore(&mut flash, &native, &SURFACE).unwrap();
        assert_eq!(restored.enabled(4), committed);
        assert!(restored.initial().is_none()); // Recovery has its own source walk.
        if committed {
            let before = flash.0.clone();
            for _ in 0..2 {
                assert_eq!(p.admission(enable).status, Status::Enabled);
            }
            assert_eq!(flash.0, before);
            assert!(!p.pending_write());
            assert_eq!(p.initial().unwrap().record, 60);
            p.bytes[6] &= 1;
            assert_eq!(p.admission(enable).status, Status::Enabled);
            assert!(p.initial().is_none());
            let disable = E::Enable {
                definition: 4,
                generation: 1,
                revision: 1,
                enabled: false,
            };
            assert_eq!(p.admission(disable).status, Status::Persisting);
            while p.pending_write() {
                p.poll(&mut flash, &native).unwrap();
            }
            assert_eq!(p.admission(enable).status, Status::Conflict);
        }
    }
    // A definition outside this build's execution capacity remains inspectable
    // and disableable; only admission to execution requires the working extent.
    let native = Device::new();
    let mut flash = Flash::new();
    let mut bytes = definition(true);
    bytes[4..6].copy_from_slice(&(crate::configLATTICE_PROGRAM_BYTES as u16).to_le_bytes());
    let mut p = install(&bytes, &native, &mut flash);
    assert_eq!(
        p.admission(E::State {
            definition: 4,
            generation: 1
        })
        .status,
        Status::Disabled
    );
    for (enabled, expected) in [(true, Status::Bounds), (false, Status::Disabled)] {
        assert_eq!(
            p.admission(E::Enable {
                definition: 4,
                generation: 1,
                revision: 0,
                enabled
            })
            .status,
            expected
        );
    }
}

#[test]
fn configuration_versions() {
    for version in [1, 2, 3, 4, 5, 6] {
        let native = Device::new();
        let mut flash = Flash::new();
        let mut p = install(&definition(true), &native, &mut flash);
        p.bytes[3] = version;
        p.checkpoint(None).unwrap();
        while p.pending_write() {
            p.poll(&mut flash, &native).unwrap();
        }
        let mut restored = Configuration::new();
        let result = restored.restore(&mut flash, &native, &SURFACE);
        assert_eq!(result.is_ok(), version <= 5);
        if version <= 5 {
            assert_eq!(&restored.bytes[..4], &MAGIC);
            assert_eq!(restored.part(4, 1).unwrap().definition.head(), Some(60));
            let before = restored.epoch();
            restored.checkpoint(None).unwrap();
            while restored.pending_write() {
                restored.poll(&mut flash, &native).unwrap();
            }
            assert!(restored.epoch() > before);
            let mut again = Configuration::new();
            again.restore(&mut flash, &native, &SURFACE).unwrap();
            assert_eq!(&again.bytes[..4], &MAGIC);
        }
    }
}

#[test]
fn clock_configuration_commit() {
    use lattice_peer_wire::clock::{Profile, Request as C, Zone};
    let native = Device::new();
    let mut flash = Flash::new();
    let mut p = install(&definition(true), &native, &mut flash);
    let end = p.used;
    let profile = Profile {
        latitude: 30_000_000,
        longitude: -97_000_000,
        zone: Zone::UTC,
    };
    for revision in 0..2 {
        let profile = Profile {
            latitude: profile.latitude + revision as i32,
            ..profile
        };
        let request = C::Configure { revision, profile };
        assert_eq!(p.clock_request(request).status, Status::Persisting);
        while p.pending_write() {
            // Every interrupted checkpoint still exposes the previous complete record.
            let mut recovered = Configuration::new();
            recovered.restore(&mut flash, &native, &SURFACE).unwrap();
            assert_eq!(
                recovered.clock().map(|c| c.0),
                (revision != 0).then_some(revision)
            );
            p.poll(&mut flash, &native).unwrap();
        }
        assert_eq!(p.clock(), Some((revision + 1, profile)));
        assert_eq!(p.clock_request(request).status, Status::Ok);
        assert_eq!(p.used, end + CLOCK_BYTES as u16);
        let mut restored = Configuration::new();
        restored.restore(&mut flash, &native, &SURFACE).unwrap();
        assert_eq!(restored.clock(), p.clock());
        assert_eq!(restored.part(4, 1).unwrap().definition.head(), Some(60));
    }
}

#[test]
fn temporal_source_boundaries() {
    use lattice_peer_wire::clock::{Predicate, Profile, Zone, MIN_UTC};
    crate::port::test_port_reset();
    activation::reset();
    let mut time = crate::kernel::time::Time::new();
    time.configure(
        Some(Profile {
            latitude: 0,
            longitude: 0,
            zone: Zone::UTC,
        }),
        0,
    );
    time.add(100, Predicate::Civil { start: 1, end: 2 }, 0)
        .unwrap();
    time.anchor(1, 0, MIN_UTC * 1000, 0).unwrap();
    for (mono, truth, emitted) in [
        (0, false, false),
        (999, false, false),
        (1000, true, true),
        (1000, true, false),
        (1999, true, false),
        (2000, false, true),
    ] {
        time.poll(mono);
        assert_eq!(time.value(100), Some(truth));
        assert_eq!(activation::pending(), emitted);
        if emitted {
            assert_eq!(
                activation::first().unwrap().cursor,
                100 | crate::kernel::time::SOURCE
            );
            activation::advance(NONE);
        }
    }
    // No native source is necessary: the temporal record is itself a local source.
    let mut bytes = terminal(0);
    bytes.truncate(48);
    bytes[10..12].copy_from_slice(&48u16.to_le_bytes());
    bytes.extend_from_slice(&[8, 0, 0, 0, 0, 1, 56, 0]); // head entry -> temporal record
    bytes.extend_from_slice(&[16, 0, 7, 48, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0]);
    assert_eq!(
        validation::validate(&bytes, &Device::new(), &SURFACE),
        Ok(())
    );
    bytes[59..61].copy_from_slice(&56u16.to_le_bytes());
    assert_eq!(
        validation::validate(&bytes, &Device::new(), &SURFACE),
        Err(Error::Link)
    );
}

struct Running {
    binding: Device,
    value: core::sync::atomic::AtomicBool,
    calls: core::sync::atomic::AtomicU32,
    last: core::sync::atomic::AtomicBool,
}
impl Native for Running {
    fn ready(&self) -> bool {
        true
    }
    fn owns(&self, instance: *const (), attribute: u64) -> bool {
        instance == core::ptr::from_ref(self).cast() && attribute == STATE
    }
    fn read(&self, reference: u64) -> StateReadResult {
        assert_eq!(reference, STATE);
        StateReadResult::Value {
            sequence: 0,
            value: Some(crate::ir::Value::boolean(
                self.value.load(Ordering::Relaxed),
            )),
        }
    }
    fn invoke(&self, reference: u64, args: &[crate::ir::Value]) -> InvokeStatus {
        assert_eq!(reference, CALL);
        assert_eq!(args.len(), 1);
        self.last
            .store(args[0].as_bool().unwrap(), Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
        InvokeStatus::Invoked
    }
    fn state_binding(&self, r: u64) -> Option<(Type, &AtomicU16)> {
        self.binding.state_binding(r)
    }
    fn transformer_signature(&self, r: u64) -> Option<&[Type]> {
        self.binding.transformer_signature(r)
    }
    fn clear_dependencies(&self) {
        self.binding.clear_dependencies();
    }
}
fn running(value: bool) -> &'static Running {
    std::boxed::Box::leak(std::boxed::Box::new(Running {
        binding: Device::new(),
        value: core::sync::atomic::AtomicBool::new(value),
        calls: core::sync::atomic::AtomicU32::new(0),
        last: core::sync::atomic::AtomicBool::new(false),
    }))
}
fn terminal(length: u16) -> Vec<u8> {
    let mut b = vec![0; 48];
    b[..4].copy_from_slice(b"LPD\x01");
    b[4..6].copy_from_slice(&length.to_le_bytes());
    b[10..12].fill(0xff);
    b[12..14].copy_from_slice(&48u16.to_le_bytes());
    b[16..48].copy_from_slice(&SURFACE);
    b.extend_from_slice(&[6, 0, 0, 0, 0, 0]);
    b
}
#[test]
fn signal_admission() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E, SIGNAL_BYTES};
    crate::port::test_port_reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(true);
    for length in [0, 1, SIGNAL_BYTES, SIGNAL_BYTES + 1, 2 * SIGNAL_BYTES, 512] {
        let mut flash = Flash::new();
        let mut p = install(&terminal(length as u16), native, &mut flash);
        let mut t = Execution::new();
        t.local = 2;
        t.epoch = 7;
        let entry = Entry {
            definition: 4,
            generation: 1,
            record: 48,
        };
        let source = Entry {
            definition: 4,
            generation: 1,
            record: 60,
        };
        let mut out = [0; DATA_BYTES];
        let bytes = vec![42; length];
        let chunks = length.div_ceil(SIGNAL_BYTES).max(1);
        for i in (0..chunks).rev() {
            let offset = i * SIGNAL_BYTES;
            let end = (offset + SIGNAL_BYTES).min(length);
            let request = E::Signal {
                source,
                target: entry,
                epoch: 3,
                sequence: 2,
                active: true,
                offset: offset as u16,
                bytes: &bytes[offset..end],
            };
            for _ in 0..2 {
                let reply = t.receive(&p, 1, request, 2, &mut out);
                assert_eq!(
                    reply.status,
                    if i == 0 {
                        Status::Accepted
                    } else {
                        Status::Receiving
                    }
                );
            }
            assert_eq!(t.statistics.admitted, u32::from(i == 0));
        }
        t.poll(&mut p, native, &context, 3);
        assert_eq!((t.statistics.admitted, t.statistics.completed), (1, 1));
        for sequence in [1, 2] {
            let end = SIGNAL_BYTES.min(length);
            assert_eq!(
                t.receive(
                    &p,
                    1,
                    E::Signal {
                        source,
                        target: entry,
                        epoch: 3,
                        sequence,
                        active: true,
                        offset: 0,
                        bytes: &bytes[..end],
                    },
                    4,
                    &mut out
                )
                .status,
                Status::Accepted
            );
            assert_eq!(t.statistics.admitted, 1);
        }
        assert_eq!(
            t.receive(
                &p,
                1,
                E::Signal {
                    source,
                    target: entry,
                    epoch: 3,
                    sequence: 3,
                    active: false,
                    offset: 0,
                    bytes: &[],
                },
                5,
                &mut out
            )
            .status,
            Status::Accepted
        );
        assert_eq!(t.statistics.admitted, 2);
        assert_eq!(
            t.receive(
                &p,
                1,
                E::Signal {
                    source,
                    target: Entry {
                        generation: 2,
                        ..entry
                    },
                    epoch: 3,
                    sequence: 4,
                    active: false,
                    offset: 0,
                    bytes: &[],
                },
                6,
                &mut out
            )
            .status,
            Status::Binding
        );
        assert_eq!(t.statistics.faults, 0);
        assert_eq!(t.wait_ms(7, true), None);
    }
}

// Exercise delivery through the real update codec, with an optional single loss.
fn settle(
    a: &mut Configuration,
    b: &mut Configuration,
    ea: &mut super::execution::Execution,
    eb: &mut super::execution::Execution,
    na: &'static Running,
    nb: &'static Running,
    context: &TaskContext,
    loss: &mut Option<bool>,
) -> (u32, u32) {
    use lattice_peer_wire::program::execution::Request as E;
    let mut counts = (0, 0);
    let mut wire = [0; lattice_peer_wire::program::transfer::REQUEST_BYTES];
    let mut out = [0; DATA_BYTES];
    for now in (0u32..20_000).step_by(100) {
        for e in [&mut *ea, &mut *eb] {
            if let Some((i, _, _, sent)) = e.awaiting() {
                if now.wrapping_sub(sent) >= 2000 {
                    e.retry(i, now);
                }
            }
        }
        ea.poll(a, na, context, now);
        eb.poll(b, nb, context, now);
        for reverse in [false, true] {
            let (from, to, p, sender, receiver, count) = if reverse {
                (&mut *eb, &mut *ea, &*a, 2, 1, &mut counts.1)
            } else {
                (&mut *ea, &mut *eb, &*b, 1, 2, &mut counts.0)
            };
            if let Some((i, owner, n)) = from.outgoing(now, &mut wire) {
                assert_eq!(owner, receiver);
                assert_eq!(wire[0], 30);
                *count += 1;
                from.sent(i, 1, now);
                let drop = loss.take();
                if drop != Some(false) {
                    let reply =
                        to.receive(p, sender, E::decode(&wire[..n]).unwrap(), now, &mut out);
                    if drop.is_none() {
                        from.reply(i, reply, now);
                    }
                }
            }
        }

        if !ea.pending() && !eb.pending() {
            return counts;
        }
    }
    panic!("bounded delivery did not settle");
}

fn conditional_pair() -> (Vec<u8>, Vec<u8>) {
    // A: owned Bool predicate, then B. B: its owned Bool predicate and a call.
    let mut head = definition(true)[..86].to_vec();
    head.extend_from_slice(&[25, 0, 5, 2, 0, 0, 0, 2]);
    head.extend_from_slice(&2u64.to_le_bytes());
    head.extend_from_slice(&4u16.to_le_bytes());
    head.extend_from_slice(&1u32.to_le_bytes());
    head.extend_from_slice(&60u16.to_le_bytes());
    head.push(0);
    let original = definition(true);
    let mut tail = original[..60].to_vec();
    tail[10..12].fill(255);
    tail.extend_from_slice(&[24, 0, 0, 2]);
    tail.extend_from_slice(&1u64.to_le_bytes());
    tail.extend_from_slice(&4u16.to_le_bytes());
    tail.extend_from_slice(&1u32.to_le_bytes());
    tail.extend_from_slice(&60u16.to_le_bytes());
    tail.extend_from_slice(&[0, 1, 84, 0]);
    let mut read = original[68..86].to_vec();
    read[16..18].copy_from_slice(&102u16.to_le_bytes());
    tail.extend_from_slice(&read);
    tail.extend_from_slice(&[11, 0, 5, 2, 0, 0, 0, 1, 113, 0, 0]);
    tail.extend_from_slice(&original[97..]);
    (head, tail)
}

#[test]
fn retained_activation() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E};
    for initial_loss in [None, Some(false), Some(true)] {
        crate::port::test_port_reset();

        crate::kernel::change::reset();
        let context = unsafe { TaskContext::assume() };
        let na = running(false);
        let nb = running(false);
        let (head, tail) = conditional_pair();
        let mut fa = Flash::new();
        let mut fb = Flash::new();
        let mut a = install(&head, na, &mut fa);
        let mut b = install(&tail, nb, &mut fb);
        assert_eq!(
            a.admission(E::Enable {
                definition: 4,
                generation: 1,
                revision: 0,
                enabled: true,
            })
            .status,
            Status::Persisting
        );
        persist(&mut a, &mut fa, na);
        let entry = Entry {
            definition: 4,
            generation: 1,
            record: 60,
        };
        let mut ea = Execution::new();
        ea.local = 1;
        ea.epoch = 10;
        let mut eb = Execution::new();
        eb.local = 2;
        eb.epoch = 20;
        assert_eq!(
            settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
            (0, 0)
        );
        assert_eq!(nb.calls.load(Ordering::Relaxed), 0);
        nb.value.store(true, Ordering::Relaxed);
        eb.activate(&b, entry).unwrap();
        assert_eq!(
            settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
            (0, 0)
        );
        assert_eq!(nb.calls.load(Ordering::Relaxed), 0);
        nb.value.store(false, Ordering::Relaxed);
        na.value.store(true, Ordering::Relaxed);
        ea.activate(&a, entry).unwrap();
        let mut loss = initial_loss;
        let (sent, returned) = settle(
            &mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut loss,
        );
        assert!(sent >= 1);
        assert_eq!(returned, 0);
        assert!(loss.is_none());
        let head_runs = ea.statistics.admitted;
        for (value, expected) in [(true, 1), (true, 1), (false, 1), (true, 2)] {
            nb.value.store(value, Ordering::Relaxed);
            eb.activate(&b, entry).unwrap();
            assert_eq!(
                settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
                (0, 0)
            );
            assert_eq!(nb.calls.load(Ordering::Relaxed), expected);
            assert_eq!(ea.statistics.admitted, head_runs);
        }
        na.value.store(false, Ordering::Relaxed);
        ea.activate(&a, entry).unwrap();
        assert_eq!(
            settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
            (1, 0)
        );
        eb.activate(&b, entry).unwrap();
        assert_eq!(
            settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
            (0, 0)
        );
        assert_eq!(nb.calls.load(Ordering::Relaxed), 2);
        na.value.store(true, Ordering::Relaxed);
        ea.activate(&a, entry).unwrap();
        assert_eq!(
            settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
            (1, 0)
        );
        assert_eq!(nb.calls.load(Ordering::Relaxed), 3);
        assert_eq!(
            a.admission(E::Enable {
                definition: 4,
                generation: 1,
                revision: 1,
                enabled: false,
            })
            .status,
            Status::Persisting
        );
        persist(&mut a, &mut fa, na);
        assert_eq!(
            settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
            (1, 0)
        );
        eb.activate(&b, entry).unwrap();
        assert_eq!(
            settle(&mut a, &mut b, &mut ea, &mut eb, na, nb, &context, &mut None),
            (0, 0)
        );
        assert_eq!(nb.calls.load(Ordering::Relaxed), 3);
        assert_eq!((ea.statistics.faults, eb.statistics.faults), (0, 0));
        assert_eq!((ea.wait_ms(0, true), eb.wait_ms(0, true)), (None, None));
    }
}

fn settle_local(
    e: &mut super::execution::Execution,
    p: &mut Configuration,
    native: &'static Running,
    context: &TaskContext,
) {
    for now in 0..16 {
        e.poll(p, native, context, now);
        if !e.pending() {
            return;
        }
    }
    panic!("local component did not become idle");
}

#[test]
fn local_connection_removal() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E};
    crate::port::test_port_reset();

    crate::kernel::change::reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(true);
    let (mut head, tail) = conditional_pair();
    head[94..102].copy_from_slice(&1u64.to_le_bytes());
    let mut flash = Flash::new();
    let mut p = install(&tail, native, &mut flash);
    let (definition, generation) = stage(&mut p, &head, 2, native);
    persist(&mut p, &mut flash, native);
    assert_eq!(
        p.admission(E::Enable {
            definition,
            generation,
            revision: 0,
            enabled: true
        })
        .status,
        Status::Persisting
    );
    persist(&mut p, &mut flash, native);
    let mut e = Execution::new();
    e.local = 1;
    e.epoch = 9;
    settle_local(&mut e, &mut p, native, &context);
    assert_eq!(native.calls.load(Ordering::Relaxed), 1);
    e.remove(definition, generation);
    assert_eq!(p.remove(definition, generation, native), Status::Persisting);
    persist(&mut p, &mut flash, native);
    settle_local(&mut e, &mut p, native, &context);
    e.activate(
        &p,
        Entry {
            definition: 4,
            generation: 1,
            record: 60,
        },
    )
    .unwrap();
    settle_local(&mut e, &mut p, native, &context);
    assert_eq!(native.calls.load(Ordering::Relaxed), 1);
    assert_eq!((e.statistics.faults, e.wait_ms(0, true)), (0, None));
}

#[test]
fn activation_and_arguments() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E};
    crate::port::test_port_reset();

    crate::kernel::change::reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(true);
    let mut bytes = terminal(1);
    bytes.truncate(48);
    bytes.extend_from_slice(&[12, 0, 0, 0, 1, 1, 0, 0, 0, 1, 60, 0]);
    bytes.extend_from_slice(&[18, 0, 6]);
    bytes.extend_from_slice(&CALL.to_le_bytes());
    bytes.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0]);
    let mut flash = Flash::new();
    let mut p = install(&bytes, native, &mut flash);
    let mut e = Execution::new();
    e.local = 2;
    e.epoch = 7;
    let source = Entry {
        definition: 8,
        generation: 1,
        record: 60,
    };
    let target = Entry {
        definition: 4,
        generation: 1,
        record: 48,
    };
    let mut out = [0; DATA_BYTES];
    for (sequence, active, value, count, last) in [
        (1, true, 1, 1, true),
        (1, true, 1, 1, true),
        (2, true, 0, 2, false),
        (1, true, 1, 2, false),
        (3, true, 0, 2, false),
        (4, false, 0, 2, false),
        (5, true, 0, 3, false),
        (6, true, 1, 4, true),
        (7, false, 0, 4, true),
    ] {
        let data = [value];
        assert_eq!(
            e.receive(
                &p,
                1,
                E::Signal {
                    source,
                    target,
                    epoch: 3,
                    sequence,
                    active,
                    offset: 0,
                    bytes: if active { &data } else { &[] },
                },
                0,
                &mut out
            )
            .status,
            Status::Accepted
        );
        settle_local(&mut e, &mut p, native, &context);
        assert_eq!(
            (
                native.calls.load(Ordering::Relaxed),
                native.last.load(Ordering::Relaxed)
            ),
            (count, last)
        );
    }
    assert_eq!(e.statistics.faults, 0);
}

#[test]
fn connection_activation_isolation() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E};
    crate::port::test_port_reset();

    crate::kernel::change::reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(true);
    let (_, tail) = conditional_pair();
    let mut flash = Flash::new();
    let mut p = install(&tail, native, &mut flash);
    let (offset, generation) = stage(&mut p, &tail, 2, native);
    persist(&mut p, &mut flash, native);
    let a = Entry {
        definition: 4,
        generation: 1,
        record: 60,
    };
    let b = Entry {
        definition: offset,
        generation,
        record: 60,
    };
    let source = Entry {
        definition: 4,
        generation: 1,
        record: 60,
    };
    let mut e = Execution::new();
    e.local = 2;
    e.epoch = 7;
    let mut out = [0; DATA_BYTES];
    for (target, sequence, active, count) in [(a, 1, true, 1), (b, 1, true, 2), (a, 2, false, 2)] {
        assert_eq!(
            e.receive(
                &p,
                1,
                E::Signal {
                    source,
                    target,
                    epoch: 3,
                    sequence,
                    active,
                    offset: 0,
                    bytes: &[],
                },
                0,
                &mut out
            )
            .status,
            Status::Accepted
        );
        settle_local(&mut e, &mut p, native, &context);
        assert_eq!(native.calls.load(Ordering::Relaxed), count);
    }
    for value in [false, true] {
        native.value.store(value, Ordering::Relaxed);
        e.activate(&p, a).unwrap();
        e.activate(&p, b).unwrap();
        settle_local(&mut e, &mut p, native, &context);
    }
    assert_eq!(native.calls.load(Ordering::Relaxed), 3);
    assert_eq!(e.statistics.faults, 0);
}

#[test]
fn branch_publications() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E};
    crate::port::test_port_reset();

    crate::kernel::change::reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(false);
    let mut bytes = definition(true)[..86].to_vec();
    bytes.extend_from_slice(&[41, 0, 5, 2, 0, 0, 0]);
    for owner in [3u64, 2] {
        bytes.push(2);
        bytes.extend_from_slice(&owner.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&60u16.to_le_bytes());
    }
    let mut flash = Flash::new();
    let mut p = install(&bytes, native, &mut flash);
    assert_eq!(
        p.admission(E::Enable {
            definition: 4,
            generation: 1,
            revision: 0,
            enabled: true
        })
        .status,
        Status::Persisting
    );
    persist(&mut p, &mut flash, native);
    let entry = Entry {
        definition: 4,
        generation: 1,
        record: 60,
    };
    let mut e = Execution::new();
    e.local = 1;
    e.epoch = 7;
    let mut wire = [0; lattice_peer_wire::program::transfer::REQUEST_BYTES];
    for (step, value, expected) in [
        (0, false, vec![(2, true)]),
        (1, true, vec![(2, false), (3, true)]),
        (2, false, vec![(2, true), (3, false)]),
    ] {
        native.value.store(value, Ordering::Relaxed);
        if step != 0 {
            e.activate(&p, entry).unwrap();
        }
        let mut emitted = Vec::new();
        for now in 0..16 {
            e.poll(&mut p, native, &context, now);
            if let Some((slot, owner, n)) = e.outgoing(now, &mut wire) {
                let E::Signal { active, .. } = E::decode(&wire[..n]).unwrap() else {
                    panic!()
                };
                emitted.push((owner, active));
                e.reply(slot, Reply::status(Status::Accepted), now);
            }
            if !e.pending() {
                break;
            }
        }
        emitted.sort_unstable();
        assert_eq!(emitted, expected);
        assert_eq!(e.statistics.faults, 0);
    }
}

#[test]
fn incoming_edge_independence() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E};
    crate::port::test_port_reset();

    crate::kernel::change::reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(true);
    let (_, bytes) = conditional_pair();
    let mut flash = Flash::new();
    let mut p = install(&bytes, native, &mut flash);
    let mut e = Execution::new();
    e.local = 3;
    e.epoch = 7;
    let target = Entry {
        definition: 4,
        generation: 1,
        record: 60,
    };
    let mut out = [0; DATA_BYTES];
    for (owner, sequence, active) in [(1, 1, true), (2, 1, true), (2, 2, false)] {
        assert_eq!(
            e.receive(
                &p,
                owner,
                E::Signal {
                    source: target,
                    target,
                    epoch: 1,
                    sequence,
                    active,
                    offset: 0,
                    bytes: &[],
                },
                0,
                &mut out
            )
            .status,
            Status::Accepted
        );
        settle_local(&mut e, &mut p, native, &context);
        assert_eq!(native.calls.load(Ordering::Relaxed), 1);
    }
    for value in [false, true] {
        native.value.store(value, Ordering::Relaxed);
        e.activate(&p, target).unwrap();
        settle_local(&mut e, &mut p, native, &context);
    }
    assert_eq!(native.calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        e.receive(
            &p,
            1,
            E::Signal {
                source: target,
                target,
                epoch: 1,
                sequence: 2,
                active: false,
                offset: 0,
                bytes: &[],
            },
            0,
            &mut out
        )
        .status,
        Status::Accepted
    );
    e.activate(&p, target).unwrap();
    settle_local(&mut e, &mut p, native, &context);
    assert_eq!(native.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn completed_call_and_withdrawal() {
    use super::execution::Execution;
    use lattice_peer_wire::program::execution::{Entry, Request as E};
    crate::port::test_port_reset();

    crate::kernel::change::reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(true);
    let (_, mut bytes) = conditional_pair();
    bytes.truncate(113);
    bytes.extend_from_slice(&[33, 0, 6]);
    bytes.extend_from_slice(&CALL.to_le_bytes());
    bytes.extend_from_slice(&[1, 0, 0, 1, 2]);
    bytes.extend_from_slice(&3u64.to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&48u16.to_le_bytes());
    bytes.push(0);
    let mut flash = Flash::new();
    let mut p = install(&bytes, native, &mut flash);
    let mut e = Execution::new();
    e.local = 2;
    e.epoch = 7;
    let target = Entry {
        definition: 4,
        generation: 1,
        record: 60,
    };
    let mut out = [0; DATA_BYTES];
    for (sequence, active) in [(1, true), (2, false)] {
        assert_eq!(
            e.receive(
                &p,
                1,
                E::Signal {
                    source: target,
                    target,
                    epoch: 1,
                    sequence,
                    active,
                    offset: 0,
                    bytes: &[],
                },
                0,
                &mut out
            )
            .status,
            Status::Accepted
        );
        e.poll(&mut p, native, &context, 0);
    }
    assert_eq!(native.calls.load(Ordering::Relaxed), 1);
    assert_eq!(native.calls.load(Ordering::Relaxed), 1);
    let mut wire = [0; lattice_peer_wire::program::transfer::REQUEST_BYTES];
    assert!(e.outgoing(0, &mut wire).is_some());
    assert_eq!(e.statistics.faults, 0);
}

/// Different continuation widths change record offsets, but not computation.
fn connected_definition(upstream: bool, owner: u64) -> Vec<u8> {
    let mut bytes = definition(true);
    // Entry's upstream Stop becomes a remote reference, shifting all later records.
    if upstream {
        bytes[10..12].fill(255);
        bytes.splice(
            63..64,
            [2].into_iter()
                .chain(owner.to_le_bytes())
                .chain(4u16.to_le_bytes())
                .chain(1u32.to_le_bytes())
                .chain(60u16.to_le_bytes()),
        );
        bytes[60..62].copy_from_slice(&24u16.to_le_bytes());
        for (at, value) in [(82, 84u16), (100, 102), (110, 113)] {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    let shift = if upstream { 16 } else { 0 };
    bytes[97 + shift..99 + shift].copy_from_slice(&33u16.to_le_bytes());
    bytes.splice(
        112 + shift..113 + shift,
        [2].into_iter()
            .chain((owner + 1).to_le_bytes())
            .chain(4u16.to_le_bytes())
            .chain(1u32.to_le_bytes())
            .chain(60u16.to_le_bytes()),
    );
    bytes
}
#[test]
fn computation_and_connections() {
    let base = definition(true);
    let code = WireDefinition::parse(&base).unwrap();
    for (upstream, owner, downstream) in [(false, 10, 11), (true, 20, 21), (true, 20, 31)] {
        let mut authored = connected_definition(upstream, owner);
        let at = 113 + if upstream { 16 } else { 0 };
        authored[at..at + 8].copy_from_slice(&u64::to_le_bytes(downstream));
        assert_eq!(
            validation::validate(&authored, &Device::new(), &SURFACE),
            Ok(())
        );
        let input = WireDefinition::parse(&authored).unwrap();
        assert!(image::same_computation(code, input));
        let mut metadata = vec![0; image::connection_len(input)];
        image::write_connections(input, 4, 1, &mut metadata);
        let bound = Definition::connected(code, &metadata).unwrap();
        assert_eq!(
            validation::validate_definition(bound, &Device::new(), &SURFACE),
            Ok(())
        );
        let mut reconstructed = vec![0; authored.len()];
        for (i, chunk) in reconstructed.chunks_mut(7).enumerate() {
            bound.copy_range(i * 7, chunk);
        }
        assert_eq!(reconstructed, authored);
        assert_eq!(
            canonical_digest(bound),
            <[u8; 32]>::from(Sha256::digest(&authored))
        );
        for end in 0..metadata.len() {
            assert!(Definition::connected(code, &metadata[..end])
                .and_then(|d| validation::validate_definition(d, &Device::new(), &SURFACE))
                .is_err());
        }
    }
    assert!(!image::same_computation(
        code,
        WireDefinition::parse(&definition(false)).unwrap()
    ));
}
#[test]
fn shared_ownership_and_reuse() {
    for removal_order in [[0, 1, 2], [1, 2, 0], [2, 0, 1]] {
        crate::port::test_port_reset();
        activation::reset();
        let native = Device::new();
        let mut flash = Flash::new();
        let mut p = install(&definition(true), &native, &mut flash);
        let mut refs = vec![(4, 1)];
        for owner in [10, 20] {
            let bytes = connected_definition(true, owner);
            let reference = stage(&mut p, &bytes, owner as u32, &native);
            assert!(p.part(reference.0, reference.1).is_err());
            persist(&mut p, &mut flash, &native);
            refs.push(reference);
        }
        let kinds: Vec<_> = image::extents(&p.bytes[..p.used as usize])
            .map(|e| e.unwrap().kind)
            .collect();
        assert_eq!(
            kinds,
            vec![image::INLINE, image::CONNECTION, image::CONNECTION]
        );
        let mut active = [true; 3];
        for index in removal_order {
            let (offset, generation) = refs[index];
            let source = p
                .part(offset, generation)
                .unwrap()
                .definition
                .sources()
                .next()
                .unwrap();
            let cursor = offset + PREFIX as u16 + source.offset;
            let next = p.dependency(cursor).unwrap().3;
            activation::record(cursor);
            let command = Request::Remove {
                definition: offset,
                generation,
            };
            assert_eq!(
                p.receive(5, command, &native, &SURFACE).status,
                Status::Persisting
            );
            assert!(p.part(offset, generation).is_err());
            assert_eq!(activation::first().unwrap().cursor, next);
            activation::advance(NONE);
            persist(&mut p, &mut flash, &native);
            assert_eq!(
                p.receive(5, command, &native, &SURFACE).status,
                Status::Removed
            );
            active[index] = false;
            p.restore(&mut flash, &native, &SURFACE).unwrap();
            for (i, (at, gen)) in refs.iter().enumerate() {
                assert_eq!(p.part(*at, *gen).is_ok(), active[i]);
            }
        }
        assert_eq!(p.used, 4);
        assert_eq!(native.head.load(Ordering::Acquire), NONE);
        let replacement = stage(&mut p, &definition(false), 99, &native);
        persist(&mut p, &mut flash, &native);
        assert_eq!(replacement.0, 4);
        assert!(replacement.1 > refs[2].1);
        assert_eq!(
            p.receive(
                5,
                Request::Remove {
                    definition: 4,
                    generation: 1
                },
                &native,
                &SURFACE
            )
            .status,
            Status::Removed
        );
        assert!(p.part(replacement.0, replacement.1).is_ok());
    }
}
#[test]
fn reclamation_commit_boundary() {
    // Every flash phase recovers either the complete old image or the complete
    // reclaimed image. Publication cannot be inferred from unchanged used size.
    for steps in 0..=5 {
        let native = Device::new();
        let mut flash = Flash::new();
        let mut p = install(&definition(true), &native, &mut flash);
        let survivor = stage(&mut p, &definition(false), 2, &native);
        persist(&mut p, &mut flash, &native);
        assert_eq!(
            p.receive(
                5,
                Request::Remove {
                    definition: 4,
                    generation: 1
                },
                &native,
                &SURFACE
            )
            .status,
            Status::Persisting
        );
        for _ in 0..steps {
            p.poll(&mut flash, &native).unwrap();
        }
        let committed = !p.pending_write();
        let mut restored = Configuration::new();
        restored.restore(&mut flash, &native, &SURFACE).unwrap();
        assert_eq!(restored.part(4, 1).is_err(), committed);
        assert!(restored.part(survivor.0, survivor.1).is_ok());
    }
}

#[test]
fn reused_extent_publication() {
    let native = Device::new();
    let mut flash = Flash::new();
    let mut p = install(&definition(true), &native, &mut flash);
    // A live record at the end bounds an internal free extent. Existing live
    // offsets remain fixed; the next installation cannot append to the image.
    let capacity = p
        .store
        .as_ref()
        .unwrap()
        .capacity()
        .min(IMAGE_CAPACITY as u32) as usize;
    let at = capacity - CLOCK_BYTES;
    let used = p.used as usize;
    image::free_header(&mut p.bytes[used..at], at - used);
    let profile = lattice_peer_wire::clock::Profile {
        latitude: 0,
        longitude: 0,
        zone: lattice_peer_wire::clock::Zone::UTC,
    };
    p.bytes[at..capacity].copy_from_slice(&clock_bytes(1, profile));
    p.used = capacity as u16;
    p.clock_offset = at as u16;
    p.checkpoint(None).unwrap();
    persist(&mut p, &mut flash, &native);
    assert_eq!(p.remove(4, 1, &native), Status::Persisting);
    persist(&mut p, &mut flash, &native);
    let baseline = flash.0.clone();
    for steps in 0..=36 {
        let mut flash = Flash(baseline.clone());
        let mut p = Configuration::new();
        p.restore(&mut flash, &native, &SURFACE).unwrap();
        let reference = stage(&mut p, &definition(false), 4, &native);
        assert_eq!(reference.0, 4);
        assert_eq!(p.used, capacity as u16);
        for _ in 0..steps {
            p.poll(&mut flash, &native).unwrap();
        }
        let committed = !p.pending_write();
        assert_eq!(p.part(reference.0, reference.1).is_ok(), committed);
        let mut restored = Configuration::new();
        restored.restore(&mut flash, &native, &SURFACE).unwrap();
        assert_eq!(restored.part(reference.0, reference.1).is_ok(), committed);
        assert_eq!(restored.clock(), Some((1, profile)));
    }
}

#[test]
fn settings_commit_boundary() {
    use core::sync::atomic::AtomicU8;
    struct Setting(AtomicU8);
    impl Native for Setting {
        fn ready(&self) -> bool {
            true
        }
        fn owns(&self, _: *const (), _: u64) -> bool {
            false
        }
        fn read(&self, _: u64) -> StateReadResult {
            panic!("no public state")
        }
        fn invoke(&self, _: u64, _: &[crate::ir::Value]) -> InvokeStatus {
            panic!("restoration is not invocation")
        }
        fn settings(&self, visit: &mut dyn FnMut(u64, Type, Option<crate::ir::Value>)) {
            visit(
                STATE,
                Type::Bool,
                Some(crate::ir::Value::boolean(
                    self.0.load(Ordering::Relaxed) != 0,
                )),
            );
        }
        fn restore_setting(
            &self,
            key: u64,
            ty: Type,
            value: Option<crate::ir::Value>,
        ) -> Result<(), crate::kernel::StateChangeError> {
            assert_eq!((key, ty), (STATE, Type::Bool));
            self.0.store(
                u8::from(value.unwrap().as_bool().unwrap()),
                Ordering::Relaxed,
            );
            Ok(())
        }
    }
    let native = Setting(AtomicU8::new(0));
    let mut flash = Flash::new();
    let mut image = Configuration::new();
    image.restore(&mut flash, &native, &SURFACE).unwrap();
    image.checkpoint_settings(&native).unwrap();
    persist(&mut image, &mut flash, &native);
    native.0.store(1, Ordering::Relaxed);
    image.checkpoint_settings(&native).unwrap();
    loop {
        let committed = !image.pending_write();
        let mut reboot = Configuration::new();
        let restored = Setting(AtomicU8::new(99));
        reboot.restore(&mut flash, &restored, &SURFACE).unwrap();
        assert_eq!(restored.0.load(Ordering::Relaxed), u8::from(committed));
        if committed {
            break;
        }
        image.poll(&mut flash, &native).unwrap();
    }
}
