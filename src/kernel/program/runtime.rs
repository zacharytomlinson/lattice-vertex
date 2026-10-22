//! Resident peer installation service. Flash stays with kernel::storage.

use super::traversal::Traversals;
use super::Programs;
use crate::kernel::appliance::Native;
use lattice_peer_wire::program::execution::Request as ExecutionRequest;
use lattice_peer_wire::program::transfer::{Reply, Request, Status, REPLY_BYTES};

static mut PROGRAMS: Programs = Programs::new();
static mut NATIVE: Option<&'static dyn Native> = None;
static mut RESTORED: bool = false;
static mut SURFACE: [u8; 32] = [0; 32];
static mut TRAVERSALS: Traversals = Traversals::new();
static mut CHECKPOINT: bool = false;

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
                let programs = &mut *core::ptr::addr_of_mut!(PROGRAMS);
                let now = milliseconds();
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
                let n = reply.encode(output);
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
            || (&*core::ptr::addr_of!(PROGRAMS)).pending_write()
            || (&*core::ptr::addr_of!(TRAVERSALS)).pending()
            || super::activation::pending()
    }
}
pub(crate) fn reconstructed() -> bool {
    unsafe {
        RESTORED
            && (&*core::ptr::addr_of!(PROGRAMS)).state == Status::Ok
            && (&*core::ptr::addr_of!(TRAVERSALS)).epoch != 0
    }
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
        let programs = &mut *core::ptr::addr_of_mut!(PROGRAMS);
        programs.expire(milliseconds());
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
            return;
        };
        crate::kernel::storage::with_storage(|storage| match storage.configuration() {
            Ok(mut region) => {
                let _ = programs.poll(&mut region, native);
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
        admit_changes(&*core::ptr::addr_of!(PROGRAMS), traversals);
        traversals.poll(
            &mut *core::ptr::addr_of_mut!(PROGRAMS),
            native,
            context,
            now,
        );
    }
}
pub(crate) fn outgoing(
    now: u32,
    out: &mut [u8; lattice_peer_wire::program::transfer::REQUEST_BYTES],
) -> Option<(usize, u64, usize)> {
    unsafe { (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).outgoing(now, out) }
}
pub(crate) fn sent(index: usize, token: u16, now: u32) {
    unsafe {
        (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).sent(index, token, now);
    }
}
pub(crate) fn awaiting() -> Option<(usize, u16, u64, u32)> {
    unsafe { (&*core::ptr::addr_of!(TRAVERSALS)).awaiting() }
}
pub(crate) fn retry(index: usize, now: u32) {
    unsafe {
        (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).retry(index, now);
    }
}
pub(crate) fn reply(index: usize, bytes: &[u8], now: u32) {
    if let Some(reply) = Reply::decode(bytes) {
        unsafe {
            (&mut *core::ptr::addr_of_mut!(TRAVERSALS)).reply(index, reply, now);
        }
    }
}

fn milliseconds() -> u32 {
    ((crate::kernel::tasks::xTaskGetTickCount() as u64 * 1000) / crate::configTICK_RATE_HZ as u64)
        as u32
}

/// Consume only this kernel instance's committed local-source notifications.
pub(super) fn admit_changes(programs: &Programs, traversals: &mut Traversals) {
    if traversals.epoch == 0 || traversals.local == 0 || !crate::kernel::change::committed() {
        return;
    }
    if let Some(change) = super::activation::first() {
        if let Some((definition, generation, source, next)) = programs.dependency(change.cursor) {
            let entry = lattice_peer_wire::program::execution::Entry {
                definition,
                generation,
                record: source.entry,
            };
            match traversals.activate(programs, entry) {
                Ok(()) => super::activation::advance(next),
                Err(Status::Busy) => {}
                Err(e) => {
                    traversals.fault(entry, e);
                    super::activation::advance(next);
                }
            }
        } else {
            super::activation::advance(u16::MAX);
        }
    }
}
