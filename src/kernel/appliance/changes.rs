//! Captured state changes awaiting the existing state task.
use super::super::state::StateChangeError;
use crate::{ir::Value, kernel::tasks, sync::TaskContext};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, Ordering},
};
const CAPACITY: usize = crate::configLATTICE_STATE_CHANGES;
const _: () = assert!(CAPACITY > 0 && crate::configLATTICE_NATIVE_STATES <= u16::MAX as usize);
#[derive(Clone, Copy)]
pub(crate) struct Captured {
    pub index: u16,
    bytes: [u8; 9],
}
impl Captured {
    const EMPTY: Self = Self {
        index: 0,
        bytes: [255; 9],
    };
    pub(crate) fn new(index: u16, value: Option<Value>) -> Self {
        let mut captured = Self {
            index,
            ..Self::EMPTY
        };
        if let Some(value) = value {
            lattice_peer_wire::scalar::encode_value(value, &mut captured.bytes);
        }
        captured
    }
    pub fn value(self) -> Option<Value> {
        (self.bytes[0] != 255).then(|| {
            lattice_peer_wire::scalar::decode_value(&self.bytes)
                .expect("typed state capture")
                .0
        })
    }
}
const _: () = assert!(core::mem::size_of::<Captured>() <= 12);
pub(crate) struct Events {
    owner: UnsafeCell<Option<&'static dyn super::Native>>,
    pending: AtomicU32,
    overflow: AtomicU32,
}
unsafe impl Sync for Events {}
impl Events {
    pub const fn new() -> Self {
        Self {
            owner: UnsafeCell::new(None),
            pending: AtomicU32::new(0),
            overflow: AtomicU32::new(0),
        }
    }
    pub fn initialize(&'static self, _context: &TaskContext, owner: &'static dyn super::Native) {
        assert_eq!(
            tasks::xTaskGetSchedulerState(),
            tasks::taskSCHEDULER_NOT_STARTED
        );
        unsafe {
            assert!(
                (*self.owner.get()).is_none(),
                "state events already initialized"
            );
            *self.owner.get() = Some(owner);
        }
    }
    pub(crate) fn send(&self, event: Captured) -> Result<(), StateChangeError> {
        super::super::state::atomic(|| {
            let owner = unsafe { (*self.owner.get()).expect("state events not initialized") };
            if super::super::work::capture(owner, event) {
                self.pending.fetch_add(1, Ordering::Relaxed);
                #[cfg(feature = "firmware-update")]
                super::super::update::activation::activity();
                Ok(())
            } else {
                self.overflow.fetch_add(1, Ordering::Relaxed);
                Err(StateChangeError::Capacity)
            }
        })
    }
    pub(crate) fn completed(&self) {
        assert!(
            self.pending.fetch_sub(1, Ordering::Relaxed) != 0,
            "uncaptured state completion"
        );
    }
    #[cfg(test)]
    pub fn take(&self, _context: &TaskContext) -> Option<Captured> {
        let owner = unsafe { (*self.owner.get()).unwrap() };
        let captured = super::super::work::take_capture(owner)?;
        self.completed();
        Some(captured)
    }
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) != 0
    }
    pub fn overflow(&self) -> u32 {
        self.overflow.load(Ordering::Relaxed)
    }
}
