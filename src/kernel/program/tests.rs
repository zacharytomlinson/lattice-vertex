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

fn install(bytes: &[u8], native: &dyn Native, flash: &mut Flash) -> Configuration {
    let mut p = Configuration::new();
    p.restore(flash, native, &SURFACE).unwrap();
    assert_eq!(
        p.receive(
            5,
            Request::Begin {
                transfer: 1,
                length: bytes.len() as u16
            },
            native,
            &SURFACE
        )
        .status,
        Status::Receiving
    );
    for (i, b) in bytes.chunks(DATA_BYTES).enumerate() {
        assert_eq!(
            p.receive(
                5,
                Request::Data {
                    transfer: 1,
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
                transfer: 1,
                digest: Sha256::digest(bytes).into()
            },
            native,
            &SURFACE
        )
        .status,
        Status::Persisting
    );
    while p.pending_write() {
        p.poll(flash, native).unwrap();
    }
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
    bytes[4..6].copy_from_slice(&(crate::configLATTICE_TRAVERSAL_BYTES as u16).to_le_bytes());
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
    for version in [1, 2, 3, 4] {
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
        assert_eq!(result.is_ok(), version <= 3);
        if version <= 3 {
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
}
impl Native for Running {
    fn ready(&self) -> bool {
        true
    }
    fn owns(&self, instance: *const (), attribute: u64) -> bool {
        instance == core::ptr::from_ref(self).cast() && attribute == STATE
    }
    fn read(&self, _: &TaskContext, reference: u64) -> StateReadResult {
        assert_eq!(reference, STATE);
        StateReadResult::Value {
            sequence: 0,
            value: Some(crate::ir::Value::boolean(
                self.value.load(Ordering::Relaxed),
            )),
        }
    }
    fn invoke(&self, _: &TaskContext, reference: u64, args: &[crate::ir::Value]) -> InvokeStatus {
        assert_eq!(reference, CALL);
        assert_eq!(args, &[crate::ir::Value::boolean(true)]);
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
fn handoff_admission() {
    use super::traversal::Traversals;
    use lattice_peer_wire::program::execution::{Cause, Entry, Request as E, Ticket, CHUNK_BYTES};
    crate::port::test_port_reset();
    let context = unsafe { TaskContext::assume() };
    let native = running(true);
    for length in [
        0,
        1,
        CHUNK_BYTES,
        CHUNK_BYTES + 1,
        2 * CHUNK_BYTES,
        crate::configLATTICE_TRAVERSAL_BYTES,
    ] {
        let mut flash = Flash::new();
        let mut p = install(&terminal(length as u16), native, &mut flash);
        let mut t = Traversals::new();
        t.epoch = 7;
        t.local = 2;
        let entry = Entry {
            definition: 4,
            generation: 1,
            record: 48,
        };
        let reserve = E::Reserve {
            epoch: 7,
            sender_epoch: 3,
            sequence: 1,
            cause: Cause {
                owner: 1,
                epoch: 3,
                sequence: 1,
            },
            entry,
            upstream: false,
            length: length as u16,
        };
        let mut out = [0; DATA_BYTES];
        let r = t.receive(&p, 1, reserve, 0, &mut out);
        assert_eq!(r.status, Status::Receiving);
        let ticket = Ticket {
            epoch: 7,
            slot: r.offset,
            generation: r.generation,
        };
        let bytes = vec![42; length];
        let commit = E::Commit {
            ticket,
            digest: Sha256::digest(&bytes).into(),
        };
        if length != 0 {
            assert_eq!(
                t.receive(&p, 1, commit, 1, &mut out).status,
                Status::Invalid
            );
        }
        for (i, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let data = E::Data {
                ticket,
                offset: (i * CHUNK_BYTES) as u16,
                bytes: chunk,
            };
            for _ in 0..2 {
                assert_eq!(
                    t.receive(&p, 1, data, 2, &mut out).status,
                    Status::Receiving
                );
            }
        }
        t.poll(&mut p, native, &context, 3);
        assert_eq!(t.statistics.admitted, 0);
        for _ in 0..2 {
            assert_eq!(
                t.receive(&p, 1, commit, 4, &mut out).status,
                Status::Accepted
            );
        }
        assert_eq!(t.statistics.admitted, 1);
        for _ in 0..crate::configLATTICE_TRAVERSALS {
            t.poll(&mut p, native, &context, 5);
        }
        assert_eq!(t.statistics.completed, 1);
        assert_eq!(
            t.receive(&p, 1, commit, 6, &mut out).status,
            Status::Accepted
        );
        assert_eq!(
            t.receive(&p, 1, E::Release { ticket }, 7, &mut out).status,
            Status::Ok
        );
        assert_eq!(t.receive(&p, 1, commit, 8, &mut out).status, Status::Stale);
        assert_eq!(
            t.receive(&p, 1, reserve, 9, &mut out).status,
            Status::Receiving
        );
        assert_eq!(t.receive(&p, 1, commit, 10, &mut out).status, Status::Stale);
        t.epoch = 8;
        assert_eq!(
            t.receive(&p, 1, reserve, 11, &mut out).status,
            Status::Stale
        );
    }
    // Busy never steals an existing context.
    let mut flash = Flash::new();
    let mut p = install(&definition(true), native, &mut flash);
    let mut t = Traversals::new();
    t.epoch = 1;
    t.local = 1;
    let entry = Entry {
        definition: 4,
        generation: 1,
        record: 60,
    };
    for _ in 0..crate::configLATTICE_TRAVERSALS {
        assert_eq!(t.activate(&p, entry), Ok(()));
    }
    assert_eq!(t.activate(&p, entry), Err(Status::Busy));
    assert_eq!(
        t.statistics.admitted,
        crate::configLATTICE_TRAVERSALS as u32
    );
    for _ in 0..crate::configLATTICE_TRAVERSALS {
        t.poll(&mut p, native, &context, 0);
    }
    assert_eq!(t.statistics.faults, 0);
}

#[test]
fn dependent_delivery() {
    use super::traversal::Traversals;
    use crate::kernel::appliance::execution;
    use lattice_peer_wire::program::execution::Request as E;
    // Each handoff phase can lose either its request or its reply. The same
    // accepted causes must still produce one invocation each on the true path.
    for upstream in [false, true] {
        for (truth, loss) in [
            (false, None),
            (true, None),
            (true, Some((9, false))),  // session request
            (true, Some((9, true))),   // session reply
            (true, Some((10, false))), // reserve request
            (true, Some((10, true))),  // reservation reply
            (true, Some((11, false))), // data request
            (true, Some((11, true))),  // data reply
            (true, Some((12, false))), // commit request
            (true, Some((12, true))),  // acceptance reply
            (true, Some((13, false))), // release request
            (true, Some((13, true))),  // release reply
        ] {
            if upstream && matches!(loss, Some((11, _))) {
                continue;
            } // upstream carries no values
            crate::port::test_port_reset();
            execution::reset();
            crate::kernel::change::reset();
            let context = unsafe { TaskContext::assume() };
            let native_a = running(truth);
            let native_b = running(truth);
            let mut head = definition(true)[..86].to_vec();
            head[4..6].copy_from_slice(&1u16.to_le_bytes());
            head[6..8].fill(0);
            head[79] = 1;
            head.extend_from_slice(&[25, 0, 5, 1, 0, 0, 0, 2]);
            head.extend_from_slice(&2u64.to_le_bytes());
            head.extend_from_slice(&4u16.to_le_bytes());
            head.extend_from_slice(&1u32.to_le_bytes());
            head.extend_from_slice(&48u16.to_le_bytes());
            head.push(0);
            head[56..58].copy_from_slice(&111u16.to_le_bytes());
            head.extend_from_slice(&[8, 0, 0, 1, 60, 0, 0, 0]);
            let mut tail = terminal(1);
            tail.truncate(48);
            tail.extend_from_slice(&[12, 0, 0, 0, 1, 1, 0, 0, 0, 1, 60, 0]);
            tail.extend_from_slice(&[18, 0, 6]);
            tail.extend_from_slice(&CALL.to_le_bytes());
            tail.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0]);
            if upstream {
                head = definition(true);
                tail = definition(true)[..60].to_vec();
                tail[4..8].fill(0);
                tail[10..12].fill(0xff);
                // A remote source entry points to A's head. It carries no operands.
                tail.extend_from_slice(&[22, 0, 0, 2]);
                tail.extend_from_slice(&1u64.to_le_bytes());
                tail.extend_from_slice(&4u16.to_le_bytes());
                tail.extend_from_slice(&1u32.to_le_bytes());
                tail.extend_from_slice(&60u16.to_le_bytes());
                tail.extend_from_slice(&[0, 0]);
            }
            let mut flash_a = Flash::new();
            let mut flash_b = Flash::new();
            let mut a = install(&head, native_a, &mut flash_a);
            let mut b = install(&tail, native_b, &mut flash_b);
            let mut ta = Traversals::new();
            ta.local = 1;
            ta.epoch = 10;
            let mut tb = Traversals::new();
            tb.local = 2;
            tb.epoch = 20;
            let mut wire = [0; lattice_peer_wire::program::transfer::REQUEST_BYTES];
            let mut data = [0; DATA_BYTES];
            let mut dropped = false;
            let source = if upstream { native_b } else { native_a };
            crate::kernel::change::bind(source);
            for now in (10..=10_000).step_by(10) {
                // Complete disabled recovery through the real handoff before enablement.
                if now == 1000 {
                    assert_eq!(
                        a.admission(E::Enable {
                            definition: 4,
                            generation: 1,
                            revision: 0,
                            enabled: true
                        })
                        .status,
                        Status::Persisting
                    );
                    while a.pending_write() {
                        a.poll(&mut flash_a, native_a).unwrap();
                    }
                }
                for t in [&mut ta, &mut tb] {
                    if let Some((index, _, _, sent)) = t.awaiting() {
                        if now - sent >= 2_000 {
                            t.retry(index, now);
                        }
                    }
                }
                if now == 1330 {
                    let (p, t) = if upstream {
                        (&b, &mut tb)
                    } else {
                        (&a, &mut ta)
                    };
                    let admitted = t.statistics.admitted;
                    let batch = crate::kernel::change::batch();
                    for sequence in 0..10 {
                        crate::kernel::change::record(
                            core::ptr::from_ref(source).cast(),
                            STATE,
                            Some(crate::ir::Value::boolean((sequence % 2 == 1) == truth)),
                        );
                    }
                    runtime::admit_changes(p, t);
                    assert_eq!(t.statistics.admitted, admitted);
                    assert_eq!(crate::kernel::change::evicted(), 2);
                    drop(batch);
                }
                if upstream {
                    runtime::admit_changes(&b, &mut tb);
                } else {
                    runtime::admit_changes(&a, &mut ta);
                }
                ta.poll(&mut a, native_a, &context, now);
                tb.poll(&mut b, native_b, &context, now);
                // Apply the same loss partition to B->A activation or A->B execution.
                let mut exchange = |from: &mut Traversals,
                                    to: &mut Traversals,
                                    p: &Configuration,
                                    sender,
                                    receiver,
                                    selected| {
                    if let Some((index, owner, n)) = from.outgoing(now, &mut wire) {
                        assert_eq!(owner, receiver);
                        from.sent(index, now as u16, now);
                        let omit = now >= 1000
                            && selected
                            && !dropped
                            && loss.is_some_and(|(opcode, _)| opcode == wire[0]);
                        dropped |= omit;
                        if !omit || loss.unwrap().1 {
                            let r = to.receive(
                                p,
                                sender,
                                E::decode(&wire[..n]).unwrap(),
                                now,
                                &mut data,
                            );
                            if !omit {
                                from.reply(index, r, now);
                            }
                        }
                    }
                };
                exchange(&mut ta, &mut tb, &b, 1, 2, !upstream);
                exchange(&mut tb, &mut ta, &a, 2, 1, upstream);
                execution::run_ready(&context);
            }
            let expected = 11 * truth as u32;
            assert_eq!(
                (
                    native_a.calls.load(Ordering::Relaxed),
                    native_b.calls.load(Ordering::Relaxed)
                ),
                if upstream {
                    (expected, 0)
                } else {
                    (0, expected)
                },
                "upstream={upstream} loss={loss:?}"
            );
            assert_eq!((ta.statistics.faults, tb.statistics.faults), (0, 0));
            assert!(a.initial().is_none());
            assert_eq!(super::activation::overflow(), 0);
            assert_eq!(dropped, loss.is_some(), "loss partition {loss:?}");
            assert!(!super::activation::pending());
            assert!(
                !(if upstream { &tb } else { &ta }).pending(),
                "sender retained completed work: {loss:?}"
            );
            crate::kernel::change::reset();
            execution::reset();
        }
    }
}
