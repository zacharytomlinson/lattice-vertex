//! Persistent owner-local program definitions and native dependency wiring.
//! Installation is distinct from admission of a distributed traversal.

pub(crate) mod activation;
mod evaluation;
mod image;
use image::Definition;
pub(crate) mod runtime;
mod traversal;
mod validation;

use super::appliance::Native;
use core::sync::atomic::Ordering;
use embedded_storage::nor_flash::NorFlash;
use lattice_peer_wire::program::{
    transfer::{Reply, Request, Status, DATA_BYTES},
    Definition as WireDefinition, Error, Source, NONE,
};
use lattice_storage::configuration::Configuration as Snapshot;
use sha2::{Digest, Sha256};

const IMAGE_CAPACITY: usize = 8192;
// One complete incoming LPD, including its header, sources and operations.
// Independent of the space reserved for all resident definitions.
const FRAGMENT_CAPACITY: usize = 512;
const _: () = assert!(IMAGE_CAPACITY < super::time::SOURCE as usize);
const PREFIX: usize = 24;
const MAGIC: [u8; 4] = *b"LPG\x04";
const CLOCK_BYTES: usize = PREFIX + lattice_peer_wire::clock::PROFILE_BYTES;

struct InterruptMask(crate::types::UBaseType_t);
impl InterruptMask {
    fn new() -> Self {
        Self(crate::port::portSET_INTERRUPT_MASK_FROM_ISR())
    }
}
impl Drop for InterruptMask {
    fn drop(&mut self) {
        crate::port::portCLEAR_INTERRUPT_MASK_FROM_ISR(self.0);
    }
}

#[derive(Clone, Copy)]
struct Part<'a> {
    offset: u16,
    generation: u32,
    caller: u64,
    transfer: u32,
    definition: Definition<'a>,
}
fn parts(bytes: &[u8]) -> impl Iterator<Item = Result<Part<'_>, Status>> {
    image::extents(bytes).filter_map(move |record| {
        let e = match record {
            Ok(e) => e,
            Err(e) => return Some(Err(status(e))),
        };
        if !matches!(e.kind, image::INLINE | image::CONNECTION) || e.flags & image::STAGING != 0 {
            return None;
        }
        let prefix = &bytes[e.at..e.at + PREFIX];
        let generation = u32::from_le_bytes(prefix[4..8].try_into().unwrap());
        Some(if generation == 0 {
            Err(Status::Invalid)
        } else {
            image::definition(bytes, e)
                .map(|definition| Part {
                    offset: e.at as u16,
                    generation,
                    caller: u64::from_le_bytes(prefix[8..16].try_into().unwrap()),
                    transfer: u32::from_le_bytes(prefix[16..20].try_into().unwrap()),
                    definition,
                })
                .map_err(status)
        })
    })
}
fn clock_bytes(revision: u32, profile: lattice_peer_wire::clock::Profile) -> [u8; CLOCK_BYTES] {
    let mut b = [0; CLOCK_BYTES];
    b[..2].copy_from_slice(&(CLOCK_BYTES as u16).to_le_bytes());
    b[3] = 1;
    b[4..8].copy_from_slice(&revision.to_le_bytes());
    profile.encode((&mut b[PREFIX..]).try_into().unwrap());
    b
}
fn context_record(
    bytes: &[u8],
) -> Result<Option<(u16, u32, lattice_peer_wire::clock::Profile)>, Status> {
    let mut found = None;
    for e in image::extents(bytes) {
        let e = e.map_err(status)?;
        if e.kind != image::CLOCK {
            continue;
        }
        let prefix = &bytes[e.at..e.at + PREFIX];
        let revision = u32::from_le_bytes(prefix[4..8].try_into().unwrap());
        if found.is_some()
            || e.span != CLOCK_BYTES
            || e.flags != 0
            || revision == 0
            || prefix[8..].iter().any(|b| *b != 0)
        {
            return Err(Status::Invalid);
        }
        found = Some((
            e.at as u16,
            revision,
            lattice_peer_wire::clock::Profile::decode(&bytes[e.at + PREFIX..e.end()])
                .ok_or(Status::Invalid)?,
        ));
    }
    Ok(found)
}
fn temporal_count(bytes: &[u8]) -> Result<usize, Status> {
    let mut count = 0;
    for p in parts(bytes) {
        for r in p?.definition.records() {
            if matches!(
                r.map_err(status)?.operation,
                lattice_peer_wire::program::Operation::Temporal { .. }
            ) {
                count += 1;
            }
        }
    }
    Ok(count)
}
fn status(error: Error) -> Status {
    match error {
        Error::Bounds | Error::Capacity => Status::Bounds,
        Error::Binding => Status::Binding,
        Error::Compatibility => Status::Compatibility,
        _ => Status::Invalid,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Receiving,
    Erasing,
    Writing(u16),
    Committing,
}
#[derive(Clone, Copy)]
struct Pending {
    caller: u64,
    transfer: u32,
    length: u16,
    accepted: u16,
    generation: u32,
    phase: Phase,
    digest: Option<[u8; 32]>,
    offset: u16,
    capacity: u16,
    image_used: u16,
}
#[derive(Clone, Copy)]
struct Checkpoint {
    target: Option<(u16, bool, u32)>,
    clock: Option<(u16, u32, lattice_peer_wire::clock::Profile)>,
    removed: Option<(u16, u32)>,
    phase: Phase,
}

pub(crate) struct Configuration {
    bytes: [u8; IMAGE_CAPACITY],
    incoming: [u8; FRAGMENT_CAPACITY],
    reply: [u8; DATA_BYTES],
    used: u16,
    store: Option<Snapshot>,
    pending: Option<Pending>,
    checkpoint_pending: Option<Checkpoint>,
    initial_pending: bool,
    clock_offset: u16,
    temporal_dirty: bool,
    last_activity: u32,
    state: Status,
}
impl Configuration {
    fn clock(&self) -> Option<(u32, lattice_peer_wire::clock::Profile)> {
        if self.clock_offset == NONE {
            return None;
        }
        let at = self.clock_offset as usize;
        Some((
            u32::from_le_bytes(self.bytes[at + 4..at + 8].try_into().unwrap()),
            lattice_peer_wire::clock::Profile::decode(&self.bytes[at + PREFIX..at + CLOCK_BYTES])
                .unwrap(),
        ))
    }
    fn clock_request(&mut self, request: lattice_peer_wire::clock::Request) -> Reply<'_> {
        use lattice_peer_wire::clock::Request as C;
        let old = self.clock();
        let revision = old.map_or(0, |c| c.0);
        if let C::Configure {
            revision: expected,
            profile,
        } = request
        {
            if !profile.valid() {
                return Reply::status(Status::Invalid);
            }
            if old.is_some_and(|(r, p)| {
                p == profile && (expected == r || expected.checked_add(1) == Some(r))
            }) {
            } else {
                if expected != revision {
                    return Reply::status(Status::Conflict);
                }
                if self.pending.is_some() || self.checkpoint_pending.is_some() {
                    return Reply::status(Status::Busy);
                }
                let Some(next) = revision.checked_add(1) else {
                    return Reply::status(Status::Bounds);
                };
                let at = if self.clock_offset == NONE {
                    self.used
                } else {
                    self.clock_offset
                };
                let total = (self.used as usize).max(at as usize + CLOCK_BYTES);
                let Some(store) = self.store.as_mut() else {
                    return Reply::status(Status::Unsupported);
                };
                if total > store.capacity().min(IMAGE_CAPACITY as u32) as usize {
                    return Reply::status(Status::Bounds);
                }
                if store.begin(total as u32).is_err() {
                    return Reply::status(Status::Storage);
                }
                self.checkpoint_pending = Some(Checkpoint {
                    target: None,
                    clock: Some((at, next, profile)),
                    removed: None,
                    phase: Phase::Erasing,
                });
                return Reply {
                    generation: next,
                    ..Reply::status(Status::Persisting)
                };
            }
        } else if request != C::Profile {
            return Reply::status(Status::Unsupported);
        }
        if let Some((generation, profile)) = old {
            profile.encode((&mut self.reply[..32]).try_into().unwrap());
            Reply {
                generation,
                bytes: &self.reply[..32],
                ..Reply::status(Status::Ok)
            }
        } else {
            Reply::status(Status::Unavailable)
        }
    }
    fn temporal_records(
        &self,
    ) -> impl Iterator<Item = (u16, lattice_peer_wire::clock::Predicate)> + '_ {
        parts(&self.bytes[..self.used as usize])
            .filter_map(Result::ok)
            .flat_map(|p| {
                p.definition.records().filter_map(move |r| {
                    let r = r.ok()?;
                    if let lattice_peer_wire::program::Operation::Temporal { predicate, .. } =
                        r.operation
                    {
                        Some((
                            p.offset
                                + PREFIX as u16
                                + p.definition.storage_record(r.offset).unwrap(),
                            predicate,
                        ))
                    } else {
                        None
                    }
                })
            })
    }
    fn temporal_entry(&self, source: u16) -> Option<lattice_peer_wire::program::execution::Entry> {
        for p in parts(&self.bytes[..self.used as usize]).filter_map(Result::ok) {
            for r in p.definition.records().filter_map(Result::ok) {
                if p.offset + PREFIX as u16 + p.definition.storage_record(r.offset)? == source {
                    if let lattice_peer_wire::program::Operation::Temporal { entry, .. } =
                        r.operation
                    {
                        return Some(lattice_peer_wire::program::execution::Entry {
                            definition: p.offset,
                            generation: p.generation,
                            record: entry,
                        });
                    }
                }
            }
        }
        None
    }
    pub(crate) const fn new() -> Self {
        Self {
            bytes: [0; IMAGE_CAPACITY],
            incoming: [0; FRAGMENT_CAPACITY],
            reply: [0; DATA_BYTES],
            used: 0,
            store: None,
            pending: None,
            checkpoint_pending: None,
            initial_pending: false,
            clock_offset: NONE,
            temporal_dirty: false,
            last_activity: 0,
            state: Status::Unsupported,
        }
    }
    pub(crate) fn restore<F: NorFlash>(
        &mut self,
        flash: &mut F,
        native: &dyn Native,
        surface: &[u8; 32],
    ) -> Result<(), Status> {
        native.clear_dependencies();
        self.pending = None;
        self.checkpoint_pending = None;
        self.initial_pending = false;
        self.clock_offset = NONE;
        self.state = Status::Storage;
        let store = Snapshot::open(flash).map_err(|_| Status::Storage)?;
        let used = store.current().map_or(4, |i| i.length() as usize);
        if !(4..=IMAGE_CAPACITY).contains(&used) {
            return Err(Status::Bounds);
        }
        if store.current().is_some() {
            store
                .read(flash, 0, &mut self.bytes[..used])
                .map_err(|_| Status::Storage)?;
        } else {
            self.bytes[..4].copy_from_slice(&MAGIC);
        }
        self.used = used as u16;
        self.store = Some(store);
        let legacy = self.bytes[..4] == *b"LPG\x01";
        let older = legacy || self.bytes[..4] == *b"LPG\x02";
        let version3 = self.bytes[..4] == *b"LPG\x03";
        if self.bytes[..4] != MAGIC && !older && !version3 {
            self.state = Status::Invalid;
            return Err(self.state);
        }
        let result = (|| {
            for e in image::extents(&self.bytes[..used]) {
                let e = e.map_err(status)?;
                if e.flags & image::STAGING != 0
                    || ((older || version3) && (e.kind > image::CLOCK || e.flags & 0xc0 != 0))
                {
                    return Err(Status::Invalid);
                }
            }
            let clock = context_record(&self.bytes[..used])?;
            if older && clock.is_some() {
                return Err(Status::Invalid);
            }
            self.clock_offset = clock.map_or(NONE, |(offset, _, _)| offset);
            if temporal_count(&self.bytes[..used])? > super::time::CAPACITY {
                return Err(Status::Bounds);
            }
            for part in parts(&self.bytes[..used]) {
                let part = part?;
                if self.bytes[part.offset as usize + 2] & 2 != 0 {
                    return Err(Status::Invalid);
                }
                if legacy
                    && (self.bytes[part.offset as usize + 2..part.offset as usize + 4] != [0, 0]
                        || self.bytes[part.offset as usize + 20..part.offset as usize + 24]
                            != [0; 4])
                {
                    return Err(Status::Invalid);
                }
                validation::validate_definition(part.definition, native, surface)
                    .map_err(status)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.state = error;
            return Err(error);
        }
        self.wire(native);
        // Header/prefix lengths and every distributed reference remain unchanged.
        // The startup checkpoint commits this one-way metadata migration.
        self.bytes[..4].copy_from_slice(&MAGIC);
        self.state = Status::Ok;
        Ok(())
    }
    fn wire(&mut self, native: &dyn Native) {
        let _mask = InterruptMask::new();
        self.temporal_dirty = true;
        native.clear_dependencies();
        let mut at = 4;
        while at < self.used as usize {
            let e = image::extent(&self.bytes[..self.used as usize], at as u16)
                .expect("validated extent");
            if matches!(e.kind, image::INLINE | image::CONNECTION) && e.flags & image::STAGING == 0
            {
                let mut index = 0;
                loop {
                    let source = image::definition(&self.bytes[..self.used as usize], e)
                        .expect("validated definition")
                        .sources()
                        .nth(index);
                    let Some(source) = source else {
                        break;
                    };
                    let cursor = at + PREFIX + source.offset as usize;
                    let (_, head) = native.state_binding(source.state).expect("validated state");
                    self.bytes[cursor + 10..cursor + 12]
                        .copy_from_slice(&head.load(Ordering::Relaxed).to_le_bytes());
                    head.store(cursor as u16, Ordering::Release);
                    index += 1;
                }
            }
            at += e.span;
        }
    }
    pub(crate) fn dependency(&self, offset: u16) -> Option<(u16, u32, Source, u16)> {
        if offset == NONE || self.state != Status::Ok {
            return None;
        }
        for part in parts(&self.bytes[..self.used as usize]) {
            let part = part.ok()?;
            let start = part.offset as usize + PREFIX;
            for source in part.definition.sources() {
                if start + source.offset as usize == offset as usize {
                    let next = u16::from_le_bytes(
                        self.bytes[offset as usize + 10..offset as usize + 12]
                            .try_into()
                            .unwrap(),
                    );
                    return Some((part.offset, part.generation, source, next));
                }
            }
        }
        None
    }
    fn progress(&self, state: Status) -> Reply<'_> {
        if let Some(p) = self.pending {
            Reply {
                status: state,
                offset: p.offset,
                generation: p.generation,
                accepted: p.accepted,
                total: p.length,
                bytes: &[],
            }
        } else {
            Reply {
                status: state,
                total: self.used,
                accepted: 0,
                ..Reply::status(state)
            }
        }
    }
    pub(crate) fn receive<'a>(
        &'a mut self,
        caller: u64,
        request: Request<'_>,
        native: &dyn Native,
        surface: &[u8; 32],
    ) -> Reply<'a> {
        if let Request::Read { offset } = request {
            if self.pending_write() {
                return Reply::status(Status::Busy);
            }
            if offset > self.used {
                return Reply::status(Status::Bounds);
            }
            let end = (offset as usize + DATA_BYTES).min(self.used as usize);
            canonical_range(
                &self.bytes[..self.used as usize],
                offset as usize,
                &mut self.reply[..end - offset as usize],
            );
            return Reply {
                status: self.state,
                offset,
                total: self.used,
                bytes: &self.reply[..end - offset as usize],
                ..Reply::status(self.state)
            };
        }
        if self.state != Status::Ok {
            return self.progress(self.state);
        }
        if let Request::Remove {
            definition,
            generation,
        } = request
        {
            return Reply {
                offset: definition,
                generation,
                ..Reply::status(self.remove(definition, generation, native))
            };
        }
        if let Request::Clock(request) = request {
            return self.clock_request(request);
        }
        if let Request::Execution(request) = request {
            return self.admission(request);
        }
        if let Request::Dependencies { state, after } = request {
            let Some((_, head)) = native.state_binding(state) else {
                return Reply::status(Status::Binding);
            };
            let cursor = if after == NONE {
                head.load(Ordering::Acquire)
            } else {
                match self.dependency(after) {
                    Some((_, _, s, next)) if s.state == state => next,
                    _ => return Reply::status(Status::Bounds),
                }
            };
            if cursor == NONE {
                return Reply::status(Status::Ok);
            }
            let Some((definition, generation, source, next)) = self.dependency(cursor) else {
                return Reply::status(Status::Invalid);
            };
            self.reply[..2].copy_from_slice(&cursor.to_le_bytes());
            self.reply[2..4].copy_from_slice(&next.to_le_bytes());
            self.reply[4..6].copy_from_slice(&source.entry.to_le_bytes());
            return Reply {
                offset: definition,
                generation,
                bytes: &self.reply[..6],
                ..Reply::status(Status::Ok)
            };
        }
        if let Request::Info = request {
            self.reply[..2].copy_from_slice(
                &(self
                    .store
                    .as_ref()
                    .unwrap()
                    .capacity()
                    .min(IMAGE_CAPACITY as u32) as u16)
                    .to_le_bytes(),
            );
            self.reply[2..4].copy_from_slice(&(validation::MAX_RECORDS as u16).to_le_bytes());
            self.reply[4..6].copy_from_slice(&(FRAGMENT_CAPACITY as u16).to_le_bytes());
            let mut reply = self.progress(self.pending.map_or(Status::Ok, |p| {
                if p.phase == Phase::Receiving {
                    Status::Receiving
                } else {
                    Status::Persisting
                }
            }));
            reply.bytes = &self.reply[..6];
            return reply;
        }
        let transfer = match request {
            Request::Begin { transfer, .. }
            | Request::Data { transfer, .. }
            | Request::Commit { transfer, .. }
            | Request::Abort { transfer } => transfer,
            _ => return Reply::status(Status::Unsupported),
        };
        if self.checkpoint_pending.is_some() {
            return Reply::status(Status::Busy);
        }
        if transfer == 0 || caller == 0 || caller == u64::MAX {
            return self.progress(Status::Invalid);
        }
        if let Some(part) = parts(&self.bytes[..self.used as usize])
            .filter_map(Result::ok)
            .find(|p| p.caller == caller && p.transfer == transfer)
        {
            let matches = match request {
                Request::Begin { length, .. } => length as usize == part.definition.len(),
                Request::Commit { digest, .. } => canonical_digest(part.definition) == digest,
                _ => false,
            };
            return Reply {
                status: if matches {
                    Status::Installed
                } else {
                    Status::Conflict
                },
                offset: part.offset,
                generation: part.generation,
                accepted: part.definition.len() as u16,
                total: part.definition.len() as u16,
                bytes: &[],
            };
        }
        if self
            .pending
            .is_some_and(|p| p.caller != caller || p.transfer != transfer)
        {
            return self.progress(Status::Busy);
        }
        let result = match request {
            Request::Begin { length, .. } => {
                if let Some(p) = self.pending {
                    if p.length != length {
                        Status::Conflict
                    } else if p.phase == Phase::Receiving {
                        Status::Receiving
                    } else {
                        Status::Persisting
                    }
                } else if (length as usize) < lattice_peer_wire::program::HEADER_BYTES
                    || length as usize > FRAGMENT_CAPACITY
                {
                    Status::Bounds
                } else {
                    let Some(generation) = self
                        .store
                        .as_ref()
                        .unwrap()
                        .current()
                        .map_or(Some(1), |i| i.generation().checked_add(1))
                        .and_then(|v| u32::try_from(v).ok())
                    else {
                        return self.progress(Status::Bounds);
                    };
                    let (at, capacity) = match image::vacancy(
                        &self.bytes[..self.used as usize],
                        self.store
                            .as_ref()
                            .unwrap()
                            .capacity()
                            .min(IMAGE_CAPACITY as u32) as usize,
                    ) {
                        Ok(v) if v.1 >= PREFIX + 12 => v,
                        _ => return self.progress(Status::Bounds),
                    };
                    self.pending = Some(Pending {
                        offset: at as u16,
                        capacity: capacity as u16,
                        image_used: self.used,
                        caller,
                        transfer,
                        length,
                        accepted: 0,
                        generation,
                        phase: Phase::Receiving,
                        digest: None,
                    });
                    Status::Receiving
                }
            }
            Request::Data { offset, bytes, .. } => {
                let Some(p) = self.pending.as_mut() else {
                    return self.progress(Status::Conflict);
                };
                let end = offset as usize + bytes.len();
                let start = 0;
                if p.phase != Phase::Receiving {
                    Status::Busy
                } else if bytes.is_empty() || bytes.len() > DATA_BYTES || end > p.length as usize {
                    Status::Bounds
                } else if end <= p.accepted as usize {
                    if self.incoming[start + offset as usize..start + end] == *bytes {
                        Status::Receiving
                    } else {
                        Status::Conflict
                    }
                } else if offset != p.accepted {
                    Status::Conflict
                } else {
                    self.incoming[start + offset as usize..start + end].copy_from_slice(bytes);
                    p.accepted = end as u16;
                    Status::Receiving
                }
            }
            Request::Commit { digest, .. } => {
                let Some(mut p) = self.pending else {
                    return self.progress(Status::Conflict);
                };
                if p.phase != Phase::Receiving {
                    if p.digest == Some(digest) {
                        Status::Persisting
                    } else {
                        Status::Conflict
                    }
                } else if p.accepted != p.length {
                    Status::Invalid
                } else {
                    let bytes = &self.incoming[..p.length as usize];
                    let hash: [u8; 32] = Sha256::digest(bytes).into();
                    if hash != digest {
                        Status::Conflict
                    } else if let Err(error) = validation::validate(bytes, native, surface) {
                        status(error)
                    } else {
                        let source = WireDefinition::parse(bytes).unwrap();
                        if temporal_count(&self.bytes[..self.used as usize]).unwrap_or(usize::MAX)
                            .saturating_add(source.records().filter(|r| matches!(r,Ok(r) if matches!(r.operation,lattice_peer_wire::program::Operation::Temporal{..}))).count()) > super::time::CAPACITY
                            || source.sources().any(|s|bytes[s.offset as usize+10..s.offset as usize+12]!=[255;2]) {
                            Status::Bounds
                        } else {
                            let shared=image::extents(&self.bytes[..self.used as usize]).filter_map(Result::ok)
                                .filter(|e|matches!(e.kind,image::INLINE|image::BODY))
                                .find_map(|e|{let generation=u32::from_le_bytes(self.bytes[e.at+4..e.at+8].try_into().unwrap());image::code(&self.bytes[..self.used as usize],e.at as u16,generation).ok()
                                    .filter(|d|image::same_computation(*d,source)).map(|_|(e.at as u16,generation))});
                            let content=shared.map_or(p.length as usize, |_|image::connection_len(source));
                            let wanted=PREFIX+content;
                            if wanted>p.capacity as usize { Status::Bounds } else {
                                let span=if p.capacity as usize-wanted<4{p.capacity as usize}else{wanted};
                                let at=p.offset as usize;
                                self.bytes[at..at+span].fill(0);
                                self.bytes[at..at+2].copy_from_slice(&(span as u16).to_le_bytes());
                                self.bytes[at+2]=(((span-wanted)as u8)<<6)|image::STAGING;
                                self.bytes[at+3]=if shared.is_some(){image::CONNECTION}else{image::INLINE};
                                self.bytes[at+4..at+8].copy_from_slice(&p.generation.to_le_bytes());
                                self.bytes[at+8..at+16].copy_from_slice(&p.caller.to_le_bytes());
                                self.bytes[at+16..at+20].copy_from_slice(&p.transfer.to_le_bytes());
                                if let Some((body,generation))=shared { image::write_connections(source,body,generation,&mut self.bytes[at+PREFIX..at+PREFIX+content]); }
                                else { self.bytes[at+PREFIX..at+PREFIX+content].copy_from_slice(bytes); }
                                let old_end=(at+p.capacity as usize).min(self.used as usize);
                                p.image_used=self.used.max((at+span)as u16);
                                if at+span<old_end {
                                    let remainder=old_end-at-span;
                                    if remainder>=4 {image::free_header(&mut self.bytes[at+span..old_end],remainder);}
                                    else { p.image_used=(at+span)as u16; }
                                }
                                if self.store.as_mut().unwrap().begin(p.image_used as u32).is_err(){ self.state=Status::Storage;Status::Storage }
                                else {p.phase=Phase::Erasing;p.digest=Some(digest);self.pending=Some(p);Status::Persisting}
                            }
                        }
                    }
                }
            }
            Request::Abort { .. } => {
                if self.pending.is_some_and(|p| p.phase != Phase::Receiving) {
                    Status::Busy
                } else {
                    self.pending = None;
                    Status::Ok
                }
            }
            _ => unreachable!(),
        };
        self.progress(result)
    }
    pub(crate) fn pending_write(&self) -> bool {
        (self.checkpoint_pending.is_some()
            || self.pending.is_some_and(|p| p.phase != Phase::Receiving))
            && self.state == Status::Ok
    }
    fn wait_ms(&self, now: u32, writable: bool) -> Option<u32> {
        if self.pending_write() {
            return writable.then_some(0);
        }
        self.pending
            .filter(|p| p.phase == Phase::Receiving)
            .map(|_| 30_000u32.saturating_sub(now.wrapping_sub(self.last_activity)))
    }
    pub(crate) fn expire(&mut self, now: u32) {
        if self.pending.is_some_and(|p| p.phase == Phase::Receiving)
            && now.wrapping_sub(self.last_activity) >= 30_000
        {
            self.pending = None;
        }
    }
    pub(crate) fn poll<F: NorFlash>(
        &mut self,
        flash: &mut F,
        native: &dyn Native,
    ) -> Result<(), Status> {
        if self.checkpoint_pending.is_some() {
            return self.poll_checkpoint(flash);
        }
        let Some(mut p) = self.pending else {
            return Ok(());
        };
        if self.state != Status::Ok {
            return Err(self.state);
        }
        let store = self.store.as_mut().unwrap();
        let total = p.image_used as usize;
        let result = (|| {
            match p.phase {
                Phase::Receiving => {}
                Phase::Erasing => {
                    if store.erase_next_page(flash)? {
                        p.phase = Phase::Writing(0);
                    }
                }
                Phase::Writing(offset) => {
                    let end = (offset as usize + 256).min(total);
                    let mut bytes = [0; 256];
                    canonical_range(
                        &self.bytes[..total],
                        offset as usize,
                        &mut bytes[..end - offset as usize],
                    );
                    store.write(flash, offset as u32, &bytes[..end - offset as usize])?;
                    p.phase = if end == total {
                        Phase::Committing
                    } else {
                        Phase::Writing(end as u16)
                    };
                }
                Phase::Committing => {
                    store.commit(flash)?;
                    return Ok(true);
                }
            }
            Ok::<bool, lattice_storage::Error>(false)
        })();
        if result.is_err() {
            self.state = Status::Storage;
            return Err(Status::Storage);
        }
        if matches!(result, Ok(true)) {
            self.used = total as u16;
            self.bytes[p.offset as usize + 2] &= !image::STAGING;
            self.pending = None;
            self.wire(native);
        } else {
            self.pending = Some(p);
        }
        Ok(())
    }
}

impl Configuration {
    fn part(&self, offset: u16, generation: u32) -> Result<Part<'_>, Status> {
        if self.state != Status::Ok {
            return Err(self.state);
        }
        parts(&self.bytes[..self.used as usize])
            .filter_map(Result::ok)
            .find(|p| p.offset == offset && p.generation == generation)
            .ok_or(Status::Binding)
    }
    fn enabled(&self, offset: u16) -> bool {
        self.bytes[offset as usize + 2] & 1 != 0
    }
    fn epoch(&self) -> u64 {
        self.store
            .as_ref()
            .and_then(|s| s.current())
            .map_or(0, |i| i.generation())
    }
    fn checkpoint(&mut self, target: Option<(u16, bool, u32)>) -> Result<(), Status> {
        if self.checkpoint_pending.is_some() || self.pending.is_some() {
            return Err(Status::Busy);
        }
        if self.state != Status::Ok {
            return Err(self.state);
        }
        self.store
            .as_mut()
            .ok_or(Status::Unsupported)?
            .begin(self.used as u32)
            .map_err(|_| Status::Storage)?;
        self.checkpoint_pending = Some(Checkpoint {
            target,
            clock: None,
            removed: None,
            phase: Phase::Erasing,
        });
        Ok(())
    }
    /// Explicit removal takes effect in this vertex's event handler. Flash commit
    /// confirms durability; it does not wait for a distributed traversal to drain.
    fn remove(&mut self, offset: u16, generation: u32, native: &dyn Native) -> Status {
        if let Some(c) = self.checkpoint_pending {
            return if c.removed == Some((offset, generation)) {
                Status::Persisting
            } else {
                Status::Busy
            };
        }
        if self.pending.is_some() {
            return Status::Busy;
        }
        if offset < 4
            || offset as usize >= IMAGE_CAPACITY
            || generation == 0
            || generation as u64 > self.epoch()
        {
            return Status::Binding;
        }
        // A repeated command for an absent generation is already satisfied. A
        // reused address with a newer generation must never be touched.
        let Ok(part) = self.part(offset, generation) else {
            return Status::Removed;
        };
        let _mask = InterruptMask::new();
        for source in part.definition.sources() {
            let cursor = offset + PREFIX as u16 + source.offset;
            let next = u16::from_le_bytes(
                self.bytes[cursor as usize + 10..cursor as usize + 12]
                    .try_into()
                    .unwrap(),
            );
            activation::redirect(cursor, next);
        }
        for r in part.definition.records().filter_map(Result::ok) {
            if matches!(
                r.operation,
                lattice_peer_wire::program::Operation::Temporal { .. }
            ) {
                let source =
                    offset + PREFIX as u16 + part.definition.storage_record(r.offset).unwrap();
                activation::redirect(source | super::time::SOURCE, NONE);
            }
        }
        self.used = image::remove(&mut self.bytes[..self.used as usize], offset)
            .expect("validated connection") as u16;
        self.wire(native);
        if let Err(e) = self.checkpoint(None) {
            self.state = e;
            return e;
        }
        self.checkpoint_pending.as_mut().unwrap().removed = Some((offset, generation));
        Status::Persisting
    }
    fn admission(
        &mut self,
        request: lattice_peer_wire::program::execution::Request<'_>,
    ) -> Reply<'_> {
        use lattice_peer_wire::program::execution::Request as E;
        let (offset, generation) = match request {
            E::State {
                definition,
                generation,
            }
            | E::Enable {
                definition,
                generation,
                ..
            } => (definition, generation),
            _ => return Reply::status(Status::Unsupported),
        };
        let part = match self.part(offset, generation) {
            Ok(p) => p,
            Err(e) => return Reply::status(e),
        };
        if part.definition.head().is_none() {
            return Reply::status(Status::Binding);
        }
        if matches!(request, E::Enable { enabled: true, .. })
            && traversal::fits(part.definition).is_err()
        {
            return Reply::status(Status::Bounds);
        }
        let revision = u32::from_le_bytes(
            self.bytes[offset as usize + 20..offset as usize + 24]
                .try_into()
                .unwrap(),
        );
        let enabled = self.enabled(offset);
        let mut status = if enabled {
            Status::Enabled
        } else {
            Status::Disabled
        };
        if let E::Enable {
            revision: expected,
            enabled: desired,
            ..
        } = request
        {
            if self.checkpoint_pending.is_some() || self.pending.is_some() {
                status = Status::Busy;
            } else if expected == revision && desired != enabled {
                status = match revision.checked_add(1) {
                    Some(next) => match self.checkpoint(Some((offset, desired, next))) {
                        Ok(()) => Status::Persisting,
                        Err(e) => e,
                    },
                    None => Status::Bounds,
                };
            } else if !((expected == revision || expected.checked_add(1) == Some(revision))
                && desired == enabled)
            {
                status = Status::Conflict;
            }
        }
        self.reply[..4].copy_from_slice(&revision.to_le_bytes());
        Reply {
            offset,
            generation,
            bytes: &self.reply[..4],
            ..Reply::status(status)
        }
    }
    fn poll_checkpoint<F: NorFlash>(&mut self, flash: &mut F) -> Result<(), Status> {
        let mut control = self.checkpoint_pending.unwrap();
        let total = control.clock.map_or(self.used as usize, |(at, _, _)| {
            (self.used as usize).max(at as usize + CLOCK_BYTES)
        });
        let result: Result<bool, lattice_storage::Error> = (|| {
            let store = self.store.as_mut().unwrap();
            match control.phase {
                Phase::Erasing => {
                    if store.erase_next_page(flash)? {
                        control.phase = Phase::Writing(0)
                    }
                }
                Phase::Writing(offset) => {
                    let end = (offset as usize + 256).min(total);
                    let mut bytes = [0; 256];
                    canonical_range(
                        &self.bytes[..self.used as usize],
                        offset as usize,
                        &mut bytes[..end - offset as usize],
                    );
                    if let Some((at, revision, profile)) = control.clock {
                        let record = clock_bytes(revision, profile);
                        for (i, value) in record.into_iter().enumerate() {
                            let position = at as usize + i;
                            if position >= offset as usize && position < end {
                                bytes[position - offset as usize] = value;
                            }
                        }
                    }
                    if let Some((target, enabled, revision)) = control.target {
                        for (at, value) in core::iter::once((
                            target as usize + 2,
                            (self.bytes[target as usize + 2] & 0xc0) | enabled as u8,
                        ))
                        .chain(
                            revision
                                .to_le_bytes()
                                .into_iter()
                                .enumerate()
                                .map(|(i, v)| (target as usize + 20 + i, v)),
                        ) {
                            if at >= offset as usize && at < end {
                                bytes[at - offset as usize] = value;
                            }
                        }
                    }
                    store.write(flash, offset as u32, &bytes[..end - offset as usize])?;
                    control.phase = if end == total {
                        Phase::Committing
                    } else {
                        Phase::Writing(end as u16)
                    };
                    return Ok(false);
                }
                Phase::Committing => {
                    store.commit(flash)?;
                    return Ok(true);
                }
                Phase::Receiving => unreachable!(),
            }
            Ok(false)
        })();
        match result {
            Err(_) => {
                self.state = Status::Storage;
                Err(Status::Storage)
            }
            Ok(false) => {
                self.checkpoint_pending = Some(control);
                Ok(())
            }
            Ok(true) => {
                if let Some((at, revision, profile)) = control.clock {
                    self.bytes[at as usize..at as usize + CLOCK_BYTES]
                        .copy_from_slice(&clock_bytes(revision, profile));
                    self.used = total as u16;
                    self.clock_offset = at;
                }
                if let Some((target, enabled, revision)) = control.target {
                    self.bytes[target as usize + 2] =
                        (self.bytes[target as usize + 2] & 0xc0) | if enabled { 3 } else { 0 };
                    self.initial_pending |= enabled;
                    self.bytes[target as usize + 20..target as usize + 24]
                        .copy_from_slice(&revision.to_le_bytes());
                }
                self.checkpoint_pending = None;
                Ok(())
            }
        }
    }
    fn initial(&mut self) -> Option<lattice_peer_wire::program::execution::Entry> {
        if !self.initial_pending {
            return None;
        }
        let entry = parts(&self.bytes[..self.used as usize])
            .filter_map(Result::ok)
            .find_map(|p| {
                (self.bytes[p.offset as usize + 2] & 2 != 0).then(|| {
                    lattice_peer_wire::program::execution::Entry {
                        definition: p.offset,
                        generation: p.generation,
                        record: p.definition.head().unwrap(),
                    }
                })
            });
        self.initial_pending = entry.is_some();
        entry
    }
    fn recovery(&self, after: u16) -> Option<(u16, lattice_peer_wire::program::execution::Entry)> {
        use lattice_peer_wire::program::execution::Entry;
        for p in parts(&self.bytes[..self.used as usize]).filter_map(Result::ok) {
            if p.definition.sources().next().is_none() && !p.definition.records().any(|r| matches!(r,Ok(r) if matches!(r.operation,lattice_peer_wire::program::Operation::Temporal {..}))) && p.offset > after && self.enabled(p.offset)
            {
                if let Some(record) = p.definition.head() {
                    return Some((
                        p.offset,
                        Entry {
                            definition: p.offset,
                            generation: p.generation,
                            record,
                        },
                    ));
                }
            }
            for s in p.definition.sources() {
                let at = p.offset + PREFIX as u16 + s.offset;
                if at > after {
                    return Some((
                        at,
                        Entry {
                            definition: p.offset,
                            generation: p.generation,
                            record: s.entry,
                        },
                    ));
                }
            }
            for r in p.definition.records().filter_map(Result::ok) {
                if let lattice_peer_wire::program::Operation::Temporal { entry, .. } = r.operation {
                    let at =
                        p.offset + PREFIX as u16 + p.definition.storage_record(r.offset).unwrap();
                    if at > after {
                        return Some((
                            at,
                            Entry {
                                definition: p.offset,
                                generation: p.generation,
                                record: entry,
                            },
                        ));
                    }
                }
            }
        }
        None
    }
}
fn canonical_digest(definition: Definition<'_>) -> [u8; 32] {
    let mut hash = Sha256::new();
    let mut b = [0; 256];
    let mut at = 0;
    while at < definition.len() {
        let n = (definition.len() - at).min(b.len());
        definition.copy_range(at, &mut b[..n]);
        hash.update(&b[..n]);
        at += n;
    }
    hash.finalize().into()
}

fn canonical_range(image: &[u8], offset: usize, output: &mut [u8]) {
    output.fill(0);
    if offset < image.len() {
        let n = output.len().min(image.len() - offset);
        output[..n].copy_from_slice(&image[offset..offset + n]);
    }
    for e in image::extents(image).filter_map(Result::ok) {
        let flags = e.at + 2;
        if flags >= offset && flags - offset < output.len() {
            output[flags - offset] &= !(2 | image::STAGING);
        }
        let definition = match e.kind {
            image::INLINE | image::BODY => Definition::parse(&image[e.at + PREFIX..e.end()]),
            image::CONNECTION => image::definition(image, e),
            _ => continue,
        };
        if let Ok(definition) = definition {
            for source in definition.sources() {
                let link = e.at + PREFIX + source.offset as usize + 10;
                for at in link..link + 2 {
                    if at >= offset && at - offset < output.len() {
                        output[at - offset] = 255;
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests;
