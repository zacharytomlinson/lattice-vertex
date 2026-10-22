//! Resident peer installation service. Flash stays with kernel::storage.

use super::Programs;
use crate::kernel::appliance::Native;
use lattice_peer_wire::program::transfer::{Reply, Request, Status, REPLY_BYTES};

static mut PROGRAMS: Programs = Programs::new();
static mut NATIVE: Option<&'static dyn Native> = None;
static mut RESTORED: bool = false;
static mut SURFACE: [u8; 32] = [0; 32];

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
    }
}
pub(crate) fn reconstructed() -> bool {
    unsafe { RESTORED && (&*core::ptr::addr_of!(PROGRAMS)).state == Status::Ok }
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
    }
}

fn milliseconds() -> u32 {
    ((crate::kernel::tasks::xTaskGetTickCount() as u64 * 1000) / crate::configTICK_RATE_HZ as u64)
        as u32
}
