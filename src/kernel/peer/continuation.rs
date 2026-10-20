//! Already-evaluated continuation packets awaiting the resident radio task.
//! Fixed transport storage, with hard oldest eviction; never graph jobs or retries.

use super::frame::{Invocation, CALL_ARGUMENT_BYTES};
use crate::{ir::Value, port, types::UBaseType_t};
use lattice_peer_wire::scalar::encode_arguments;

const CAPACITY: usize = 4;
#[derive(Clone, Copy)]
pub(super) struct Packet {
    owner: u64,
    transformer: u64,
    request: u32,
    arguments: [u8; CALL_ARGUMENT_BYTES],
    length: u8,
}
impl Packet {
    pub fn call(&self, local: u64) -> Invocation {
        Invocation {
            caller: local,
            owner: self.owner,
            request: self.request,
            transformer: self.transformer,
        }
    }
    pub fn arguments(&self) -> &[u8] {
        &self.arguments[..usize::from(self.length)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubmitError {
    NotReady,
    Target,
    Arguments,
    Exhausted,
}

struct Outgoing {
    local: u64,
    next_request: u32,
    #[cfg(feature = "host-link")]
    issued: bool,
    frames: [Option<Packet>; CAPACITY],
    first: usize,
    count: usize,
    evicted: u32,
    #[cfg(feature = "host-link")]
    host_ids: [(u32, u32); 8],
    #[cfg(feature = "host-link")]
    next_host: usize,
}
impl Outgoing {
    const fn new() -> Self {
        Self {
            local: 0,
            next_request: 0,
            #[cfg(feature = "host-link")]
            issued: false,
            frames: [None; CAPACITY],
            first: 0,
            count: 0,
            evicted: 0,
            #[cfg(feature = "host-link")]
            host_ids: [(0, 0); 8],
            #[cfg(feature = "host-link")]
            next_host: 0,
        }
    }
    fn allocate(&mut self) -> Option<u32> {
        if self.next_request == 0 {
            return None;
        }
        let request = self.next_request;
        #[cfg(feature = "host-link")]
        {
            self.issued = true;
        }
        self.next_request = request.checked_add(1).unwrap_or(0);
        Some(request)
    }
}
// Includes both transport packets and optional host-token mappings.
const _: () = assert!(core::mem::size_of::<Outgoing>() <= 640);
static mut OUTGOING: Outgoing = Outgoing::new();
fn access<R>(f: impl FnOnce(&mut Outgoing) -> R) -> R {
    struct Restore(UBaseType_t);
    impl Drop for Restore {
        fn drop(&mut self) {
            port::portCLEAR_INTERRUPT_MASK_FROM_ISR(self.0);
        }
    }
    let _restore = Restore(port::portSET_INTERRUPT_MASK_FROM_ISR());
    // Supported ports are single-core. No radio or appliance work runs here.
    f(unsafe { &mut *core::ptr::addr_of_mut!(OUTGOING) })
}

pub(crate) fn start(local: u64, seed: u32) {
    assert!(local != 0 && local != u64::MAX);
    access(|out| {
        assert_eq!(out.local, 0, "peer continuation transport already started");
        out.local = local;
        out.next_request = (seed & 0x7fff_ffff).max(1);
    });
}

pub(crate) fn submit(owner: u64, transformer: u64, values: &[Value]) -> Result<(), SubmitError> {
    let valid = |v| v != 0 && v != u64::MAX;
    if !valid(owner) || !valid(transformer) {
        return Err(SubmitError::Target);
    }
    let mut frame = Packet {
        owner,
        transformer,
        request: 0,
        arguments: [0; CALL_ARGUMENT_BYTES],
        length: 0,
    };
    frame.length =
        encode_arguments(values, &mut frame.arguments).ok_or(SubmitError::Arguments)? as u8;
    access(|out| {
        if out.local == 0 {
            return Err(SubmitError::NotReady);
        }
        if owner == out.local {
            return Err(SubmitError::Target);
        }
        frame.request = out.allocate().ok_or(SubmitError::Exhausted)?;
        if out.count == CAPACITY {
            out.first = (out.first + 1) % CAPACITY;
            out.count -= 1;
            out.evicted = out.evicted.wrapping_add(1);
        }
        out.frames[(out.first + out.count) % CAPACITY] = Some(frame);
        out.count += 1;
        Ok(())
    })?;
    #[cfg(feature = "firmware-update")]
    crate::kernel::update::activation::activity();
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    crate::kernel::radio::task::notify_work();
    Ok(())
}

pub(crate) fn pending() -> bool {
    access(|out| out.count != 0)
}
pub(crate) fn evicted() -> u32 {
    access(|out| out.evicted)
}
pub(super) fn take() -> Option<Packet> {
    access(|out| {
        if out.count == 0 {
            return None;
        }
        let frame = out.frames[out.first].take();
        out.first = (out.first + 1) % CAPACITY;
        out.count -= 1;
        frame
    })
}

/// USB tokens remain host identities. Radio IDs share the fragment allocator;
/// the most recent eight host tokens preserve their ID across USB reconnects.
#[cfg(feature = "host-link")]
pub(super) fn host_request(token: u32) -> Option<u32> {
    access(|out| {
        if out.local == 0 {
            return None;
        }
        if let Some(&(_, id)) = out.host_ids.iter().find(|&&(t, id)| id != 0 && t == token) {
            return Some(id);
        }
        // USB-only companions retain entropy from the client's first call token.
        // Once any origin has allocated, all origins advance one shared counter.
        if !out.issued {
            out.next_request = (token & 0x7fff_ffff).max(1);
        }
        let id = out.allocate()?;
        out.host_ids[out.next_host] = (token, id);
        out.next_host = (out.next_host + 1) % out.host_ids.len();
        Some(id)
    })
}

#[cfg(test)]
pub(crate) fn reset() {
    access(|out| *out = Outgoing::new());
}

#[cfg(all(test, feature = "port-test"))]
mod tests;
