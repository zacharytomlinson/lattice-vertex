//! Resident peer installation service. Flash stays with kernel::storage.

use super::traversal::Traversals;
use super::Configuration;
use crate::kernel::appliance::Native;
use lattice_peer_wire::program::execution::Request as ExecutionRequest;
use lattice_peer_wire::program::transfer::{Reply, Request, Status, REPLY_BYTES};

static mut CONFIGURATION: Configuration = Configuration::new();
static mut NATIVE: Option<&'static dyn Native> = None;
static mut RESTORED: bool = false;
static mut SURFACE: [u8; 32] = [0; 32];
static mut TRAVERSALS: Traversals = Traversals::new();
static mut CHECKPOINT: bool = false;
static mut FLASH_BLOCKED: bool = false;
static mut MORE_LOCAL_WORK: bool = false;
static mut TIME: crate::kernel::time::Time = crate::kernel::time::Time::new();
static mut PROFILE_REVISION: u32 = 0;
struct Synchronization {
    waiting: Option<(u16, u64, u32, u64)>,
    after: u64,
    due: u64,
}
static mut SYNC: Synchronization = Synchronization {
    waiting: None,
    after: 0,
    due: 0,
};
#[derive(Clone, Copy)]
pub(crate) enum Work {
    Visit(usize),
    Time,
}

pub(crate) fn initialize(native: &'static dyn Native, surface: [u8; 32]) {
    assert_eq!(
        crate::kernel::tasks::xTaskGetSchedulerState(),
        crate::kernel::tasks::taskSCHEDULER_NOT_STARTED
    );
    unsafe {
        assert!(
            (&*core::ptr::addr_of!(NATIVE)).is_none(),
            "program service already initialized"
        );
        core::ptr::addr_of_mut!(NATIVE).write(Some(native));
        core::ptr::addr_of_mut!(SURFACE).write(surface);
    }
}
pub(crate) fn receive(caller: u64, request: Request<'_>, output: &mut [u8; REPLY_BYTES]) -> usize {
    unsafe {
        match *core::ptr::addr_of!(NATIVE) {
            Some(native) => {
                let programs = &mut *core::ptr::addr_of_mut!(CONFIGURATION);
                let now = milliseconds();
                // Earlier admitted local evaluations run before this command.
                // Native calls and remote handoffs yield independently.
                let traversals = &mut *core::ptr::addr_of_mut!(TRAVERSALS);
                let context = crate::sync::TaskContext::assume();
                traversals.dispatch(programs, native, &context);

                if let Request::Clock(operation) = request {
                    use lattice_peer_wire::clock::Request as C;
                    if matches!(operation, C::Read | C::Anchor { .. }) {
                        let epoch = (&*core::ptr::addr_of!(TRAVERSALS)).epoch;
                        if epoch == 0 {
                            return Reply::status(Status::Busy).encode(output);
                        }
                        let time = &mut *core::ptr::addr_of_mut!(TIME);
                        let mono = crate::kernel::time::monotonic_ms();
                        if let C::Anchor {
                            epoch: expected,
                            revision,
                            utc_ms,
                        } = operation
                        {
                            let result = if expected != epoch {
                                Err(Status::Stale)
                            } else {
                                time.anchor(caller, revision, utc_ms, mono)
                            };
                            return Reply {
                                generation: time.revision(),
                                ..Reply::status(result.err().unwrap_or(Status::Ok))
                            }
                            .encode(output);
                        }
                        let mut data = [0; 16];
                        data[..8].copy_from_slice(&epoch.to_le_bytes());
                        let utc = time.utc(mono);
                        if let Some(utc) = utc {
                            data[8..].copy_from_slice(&utc.to_le_bytes());
                        }
                        return Reply {
                            generation: time.revision(),
                            bytes: &data[..if utc.is_some() { 16 } else { 8 }],
                            ..Reply::status(if utc.is_some() {
                                Status::Ok
                            } else {
                                Status::Unavailable
                            })
                        }
                        .encode(output);
                    }
                }
                if let Request::Execution(operation) = request {
                    let traversals = &mut *core::ptr::addr_of_mut!(TRAVERSALS);
                    if traversals.epoch == 0 {
                        return Reply::status(if programs.state == Status::Ok {
                            Status::Busy
                        } else {
                            programs.state
                        })
                        .encode(output);
                    }
                    if !matches!(
                        operation,
                        ExecutionRequest::State { .. } | ExecutionRequest::Enable { .. }
                    ) {
                        let mut data = [0; lattice_peer_wire::program::transfer::DATA_BYTES];
                        return traversals
                            .receive(programs, caller, operation, now, &mut data)
                            .encode(output);
                    }
                }
                programs.expire(now);
                let reply =
                    programs.receive(caller, request, native, &*core::ptr::addr_of!(SURFACE));
                let active = reply.status == Status::Receiving
                    && matches!(request, Request::Begin { .. } | Request::Data { .. });
                let status = reply.status;
                let n = reply.encode(output);
                if let Request::Remove {
                    definition,
                    generation,
                } = request
                {
                    if matches!(
                        status,
                        Status::Persisting | Status::Removed | Status::Storage
                    ) {
                        (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).remove(definition, generation);
                        (&mut *core::ptr::addr_of_mut!(TIME))
                            .retain(|source| programs.temporal_entry(source).is_some());
                    }
                }

                if active {
                    programs.last_activity = now;
                }
                n
            }
            None => Reply::status(Status::Unsupported).encode(output),
        }
    }
}
pub(crate) fn pending() -> bool {
    unsafe {
        (!RESTORED && (&*core::ptr::addr_of!(NATIVE)).is_some())
            || (&*core::ptr::addr_of!(CONFIGURATION)).pending_write()
            || (&*core::ptr::addr_of!(TRAVERSALS)).pending()
            || super::activation::pending()
    }
}
pub(crate) fn reconstructed() -> bool {
    unsafe { RESTORED && (&*core::ptr::addr_of!(CONFIGURATION)).state == Status::Ok }
}
#[inline(never)]
pub(crate) fn poll() {
    #[cfg(not(any(feature = "nrf52840-update", feature = "esp32c6-update")))]
    unsafe {
        RESTORED = true;
    }
    #[cfg(any(feature = "nrf52840-update", feature = "esp32c6-update"))]
    unsafe {
        let Some(native) = *core::ptr::addr_of!(NATIVE) else {
            return;
        };
        let programs = &mut *core::ptr::addr_of_mut!(CONFIGURATION);
        programs.expire(milliseconds());
        if FLASH_BLOCKED {
            return;
        }
        if !RESTORED {
            crate::kernel::storage::with_storage(|storage| match storage.configuration() {
                Ok(mut region) => {
                    let _ = programs.restore(&mut region, native, &*core::ptr::addr_of!(SURFACE));
                }
                Err(_) => programs.state = Status::Storage,
            });
            RESTORED = true;
            if programs.state == Status::Ok {
                CHECKPOINT = programs.checkpoint(None).is_ok();
            }
        }
        if !programs.pending_write() {
            return;
        }
        #[cfg(feature = "firmware-update")]
        let Some(_flash) = crate::kernel::update::staging::FlashStep::enter() else {
            FLASH_BLOCKED = true;
            return;
        };
        crate::kernel::storage::with_storage(|storage| match storage.configuration() {
            Ok(mut region) => {
                // Trial firmware reconstructs read-only configuration before
                // confirmation. Only then may its new incarnation be committed;
                // execution remains gated on that commit, not confirmation alone.
                if region.is_writable() {
                    let _ = programs.poll(&mut region, native);
                } else {
                    FLASH_BLOCKED = true;
                }
            }
            Err(_) => programs.state = Status::Storage,
        });
        if CHECKPOINT && programs.checkpoint_pending.is_none() && programs.state == Status::Ok {
            (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).epoch = programs.epoch();
            CHECKPOINT = false;
        }
    }
}

pub(crate) fn poll_work(local: u64, context: &crate::sync::TaskContext, now: u32) {
    unsafe {
        let Some(native) = *core::ptr::addr_of!(NATIVE) else {
            return;
        };
        let traversals = &mut *core::ptr::addr_of_mut!(TRAVERSALS);
        traversals.local = local;
        {
            let programs = &mut *core::ptr::addr_of_mut!(CONFIGURATION);
            let time = &mut *core::ptr::addr_of_mut!(TIME);
            let mono = crate::kernel::time::monotonic_ms();
            let profile = programs.clock();
            let revision = profile.map_or(0, |p| p.0);
            if PROFILE_REVISION != revision {
                time.configure(profile.map(|p| p.1), mono);
                PROFILE_REVISION = revision;
                traversals.wake();
            }
            if programs.temporal_dirty {
                time.retain(|source| programs.temporal_entry(source).is_some());
                for (source, predicate) in programs.temporal_records() {
                    time.add(source, predicate, mono)
                        .expect("validated temporal capacity");
                }
                programs.temporal_dirty = false;
            }
            if time.wait_ms(mono) == Some(0) {
                traversals.wake();
            }
            time.poll(mono);
        }
        MORE_LOCAL_WORK = admit_changes(&*core::ptr::addr_of!(CONFIGURATION), traversals)
            && super::activation::pending();
        MORE_LOCAL_WORK |= traversals.poll(
            &mut *core::ptr::addr_of_mut!(CONFIGURATION),
            native,
            context,
            now,
        );
    }
}
pub(crate) fn outgoing(
    now: u32,
    out: &mut [u8; lattice_peer_wire::program::transfer::REQUEST_BYTES],
    neighbor: impl FnOnce(u64) -> Option<u64>,
) -> Option<(Work, u64, usize)> {
    unsafe {
        if awaiting().is_some() {
            return None;
        }
        let mono = crate::kernel::time::monotonic_ms();
        let time = &*core::ptr::addr_of!(TIME);
        let sync = &mut *core::ptr::addr_of_mut!(SYNC);
        if time.profile.is_some() && time.utc(mono).is_none() && mono >= sync.due {
            if let Some(peer) = neighbor(sync.after) {
                let n = Request::Clock(lattice_peer_wire::clock::Request::Read).encode(out);
                return Some((Work::Time, peer, n));
            }
            sync.after = 0;
            sync.due = mono + 5000;
        }
        (&mut *core::ptr::addr_of_mut!(TRAVERSALS))
            .outgoing(now, out)
            .map(|(i, o, n)| (Work::Visit(i), o, n))
    }
}
pub(crate) fn sent(work: Work, owner: u64, token: u16, now: u32) {
    unsafe {
        match work {
            Work::Visit(index) => {
                (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).sent(index, token, now)
            }
            Work::Time => {
                (&mut *core::ptr::addr_of_mut!(SYNC)).waiting =
                    Some((token, owner, now, crate::kernel::time::monotonic_ms()))
            }
        }
    }
}
pub(crate) fn awaiting() -> Option<(Work, u16, u64, u32)> {
    unsafe {
        (&*core::ptr::addr_of!(SYNC))
            .waiting
            .map(|(t, o, n, _)| (Work::Time, t, o, n))
            .or_else(|| {
                (&*core::ptr::addr_of!(TRAVERSALS))
                    .awaiting()
                    .map(|(i, t, o, n)| (Work::Visit(i), t, o, n))
            })
    }
}
pub(crate) fn retry(work: Work, now: u32) {
    unsafe {
        match work {
            Work::Visit(index) => (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).retry(index, now),
            Work::Time => {
                let sync = &mut *core::ptr::addr_of_mut!(SYNC);
                if let Some((_, owner, _, _)) = sync.waiting {
                    sync.after = owner;
                }
                sync.waiting = None;
                sync.due = crate::kernel::time::monotonic_ms() + 500;
            }
        }
    }
}
pub(crate) fn reply(work: Work, bytes: &[u8], now: u32) {
    if let Some(reply) = Reply::decode(bytes) {
        unsafe {
            match work {
                Work::Visit(index) => {
                    (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).reply(index, reply, now)
                }
                Work::Time => {
                    let mono = crate::kernel::time::monotonic_ms();
                    let time = &mut *core::ptr::addr_of_mut!(TIME);
                    if let Some((_, owner, _, sent)) = (&*core::ptr::addr_of!(SYNC)).waiting {
                        if reply.status == Status::Ok
                            && reply.bytes.len() == 16
                            && time.utc(mono).is_none()
                        {
                            let utc = i64::from_le_bytes(reply.bytes[8..].try_into().unwrap());
                            if let Some(utc) =
                                utc.checked_add((mono.saturating_sub(sent) / 2) as i64)
                            {
                                let _ = time.anchor(owner, time.revision(), utc, mono);
                            }
                        }
                    }
                    retry(Work::Time, now);
                }
            }
        }
    }
}
pub(crate) fn temporal_value(source: u16) -> Option<bool> {
    unsafe { (&*core::ptr::addr_of!(TIME)).value(source) }
}
pub(crate) fn resume() {
    unsafe {
        FLASH_BLOCKED = false;
        (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).wake();
    }
}
pub(crate) fn wait_ms(now: u32) -> Option<u64> {
    unsafe {
        let programs = &*core::ptr::addr_of!(CONFIGURATION);
        let traversals = &*core::ptr::addr_of!(TRAVERSALS);
        let time = &*core::ptr::addr_of!(TIME);
        let sync = &*core::ptr::addr_of!(SYNC);
        let mono = crate::kernel::time::monotonic_ms();
        let mut next = time.wait_ms(mono);
        let mut include = |ms: u64| next = Some(next.map_or(ms, |n| n.min(ms)));
        if MORE_LOCAL_WORK || (!RESTORED && (&*core::ptr::addr_of!(NATIVE)).is_some()) {
            include(0);
        }
        if let Some(ms) = programs.wait_ms(now, !FLASH_BLOCKED) {
            include(ms.into());
        }
        if let Some(ms) = traversals.wait_ms(now, sync.waiting.is_none()) {
            include(ms.into());
        }
        if let Some((_, _, sent, _)) = sync.waiting {
            include(2000u32.saturating_sub(now.wrapping_sub(sent)).into());
        } else if traversals.awaiting().is_none()
            && time.profile.is_some()
            && time.utc(mono).is_none()
        {
            include(sync.due.saturating_sub(mono));
        }
        next
    }
}

fn milliseconds() -> u32 {
    ((crate::kernel::tasks::xTaskGetTickCount() as u64 * 1000) / crate::configTICK_RATE_HZ as u64)
        as u32
}

/// Consume only this kernel instance's committed local-source notifications.
pub(super) fn admit_changes(programs: &Configuration, traversals: &mut Traversals) -> bool {
    if traversals.epoch == 0 || traversals.local == 0 || !crate::kernel::change::committed() {
        return false;
    }
    if let Some(change) = super::activation::first() {
        if change.cursor & crate::kernel::time::SOURCE != 0 {
            if let Some(entry) =
                programs.temporal_entry(change.cursor & !crate::kernel::time::SOURCE)
            {
                match traversals.activate(programs, entry) {
                    Err(Status::Busy) => return false,
                    Err(e) => traversals.fault(entry, e),
                    Ok(()) => {}
                }
            }
            super::activation::advance(u16::MAX);
            return true;
        }
        if let Some((definition, generation, source, next)) = programs.dependency(change.cursor) {
            let entry = lattice_peer_wire::program::execution::Entry {
                definition,
                generation,
                record: source.entry,
            };
            match traversals.activate(programs, entry) {
                Ok(()) => super::activation::advance(next),
                Err(Status::Busy) => return false,
                Err(e) => {
                    traversals.fault(entry, e);
                    super::activation::advance(next);
                }
            }
        } else {
            super::activation::advance(u16::MAX);
        }
        return true;
    }
    false
}

pub(crate) fn epoch() -> u64 {
    unsafe { (&*core::ptr::addr_of!(TRAVERSALS)).epoch }
}
