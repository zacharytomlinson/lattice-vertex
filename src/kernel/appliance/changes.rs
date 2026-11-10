//! Captured state changes awaiting the existing state task.
use super::super::state::StateChangeError;
use crate::{
    ir::Value,
    kernel::{queue::StaticQueue_t, tasks},
    port,
    sync::{Queue, TaskContext},
    types::*,
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
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
    queue: UnsafeCell<Option<Queue<Captured>>>,
    control: UnsafeCell<StaticQueue_t>,
    storage: UnsafeCell<[Captured; CAPACITY]>,
    initialized: AtomicBool,
    overflow: AtomicU32,
}
unsafe impl Sync for Events {}
impl Events {
    pub const fn new() -> Self {
        Self {
            queue: UnsafeCell::new(None),
            control: UnsafeCell::new(StaticQueue_t::new()),
            storage: UnsafeCell::new([Captured::EMPTY; CAPACITY]),
            initialized: AtomicBool::new(false),
            overflow: AtomicU32::new(0),
        }
    }
    pub fn initialize(&'static self, context: &TaskContext) {
        assert_eq!(
            tasks::xTaskGetSchedulerState(),
            tasks::taskSCHEDULER_NOT_STARTED
        );
        assert!(
            !self.initialized.load(Ordering::Acquire),
            "state events already initialized"
        );
        unsafe {
            *self.queue.get() = Some(
                Queue::new_static(context, &mut *self.storage.get(), &mut *self.control.get())
                    .expect("state event queue"),
            );
        }
        self.initialized.store(true, Ordering::Release);
    }
    fn queue(&self) -> &Queue<Captured> {
        assert!(
            self.initialized.load(Ordering::Acquire),
            "state events not initialized"
        );
        unsafe { (*self.queue.get()).as_ref().unwrap() }
    }
    pub(crate) fn send(&self, event: Captured) -> Result<(), StateChangeError> {
        let sent = if port::xPortIsInsideInterrupt() != pdFALSE {
            let mut woken = pdFALSE;
            let sent = unsafe { self.queue().send_from_isr(&event, &mut woken) };
            port::portYIELD_FROM_ISR(woken);
            sent
        } else {
            self.queue()
                .try_send(&unsafe { TaskContext::assume() }, &event)
        };
        if sent {
            #[cfg(feature = "firmware-update")]
            crate::kernel::update::activation::activity();
            Ok(())
        } else {
            self.overflow.fetch_add(1, Ordering::Relaxed);
            Err(StateChangeError::Capacity)
        }
    }
    pub fn take(&self, context: &TaskContext) -> Option<Captured> {
        self.queue().try_receive(context)
    }
    pub fn pending(&self, context: &TaskContext) -> bool {
        !self.queue().is_empty(context)
    }
    pub fn wait(&self, context: &TaskContext) {
        let _ = self.queue().peek(context);
    }
    pub fn overflow(&self) -> u32 {
        self.overflow.load(Ordering::Relaxed)
    }
}
