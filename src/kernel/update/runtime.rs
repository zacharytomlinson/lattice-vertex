//! The resident peer task exclusively owns flash and the receiver. Local control
//! requests use one atomic notification, not an appliance-owned networking loop.
use super::{package::Policy, Receiver, Storage};
use crate::{kernel::tasks, port, sync::TaskContext};
use core::sync::atomic::{AtomicU8, Ordering};
use lattice_peer_wire::update::{Progress, Request, Status};

#[cfg(feature = "port-nrf52840")]
pub type PlatformStorage = crate::port::nrf52840::update::ImageStorage<'static>;
#[cfg(feature = "port-nrf52840")]
type PortError = crate::port::nrf52840::update::Error;
#[cfg(feature = "port-esp32c6")]
pub type PlatformStorage = crate::port::esp32c6::update::ImageStorage<'static>;
#[cfg(feature = "port-esp32c6")]
type PortError = crate::port::esp32c6::update::Error;

impl Storage for PlatformStorage {
    type Error = PortError;
    fn capacity(&self) -> u32 {
        self.image_capacity()
    }
    fn journal_capacity(&self) -> u32 {
        PlatformStorage::journal_capacity(self)
    }
    fn journal_read(&mut self, o: u32, b: &mut [u8]) -> Result<(), Self::Error> {
        PlatformStorage::journal_read(self, o, b)
    }
    fn journal_erase(&mut self, p: u32) -> Result<(), Self::Error> {
        PlatformStorage::journal_erase(self, p)
    }
    fn journal_write(&mut self, o: u32, b: &[u8]) -> Result<(), Self::Error> {
        PlatformStorage::journal_write(self, o, b)
    }
    fn resume(&mut self, n: u32, o: u32) -> Result<(), Self::Error> {
        PlatformStorage::resume(self, n, o)
    }
    fn erase_next_page(&mut self) -> Result<bool, Self::Error> {
        PlatformStorage::erase_next_page(self)
    }
    fn write(&mut self, o: u32, b: &[u8]) -> Result<(), Self::Error> {
        PlatformStorage::write(self, o, b)
    }
    fn verify(&mut self, d: &[u8; 32]) -> Result<(), Self::Error> {
        self.verify_image(d)
    }
    fn select_trial(&mut self, d: &[u8; 32]) -> Result<(), Self::Error> {
        PlatformStorage::select_trial(self, d)
    }
    fn confirm(&mut self) -> Result<(), Self::Error> {
        PlatformStorage::confirm(self)
    }
}

#[derive(Clone, Copy)]
pub struct Snapshot {
    pub progress: Progress,
    pub control_result: Status,
    pub control_pending: bool,
    pub release: u64,
}
static mut RECEIVER: Option<Receiver<PlatformStorage>> = None;
static CONTROL: AtomicU8 = AtomicU8::new(0);
static mut SNAPSHOT: Snapshot = Snapshot {
    progress: Progress {
        package: [0; 16],
        phase: lattice_peer_wire::update::Phase::Idle,
        status: Status::Unsupported,
        accepted: 0,
        committed: 0,
        prepared: 0,
    },
    control_result: Status::Ok,
    control_pending: false,
    release: 0,
};

pub(crate) fn initialize(storage: PlatformStorage, policy: Policy) {
    assert_eq!(
        tasks::xTaskGetSchedulerState(),
        tasks::taskSCHEDULER_NOT_STARTED
    );
    assert!(
        storage.journal_capacity() >= 4096,
        "update journal is not provisioned"
    );
    unsafe {
        assert!(
            (*core::ptr::addr_of!(RECEIVER)).is_none(),
            "updater already initialized"
        );
        core::ptr::addr_of_mut!(RECEIVER).write(Some(Receiver::new(storage, policy)));
        publish(Status::Ok);
    }
}

unsafe fn publish(control_result: Status) {
    let Some(receiver) = (&*core::ptr::addr_of!(RECEIVER)).as_ref() else {
        return;
    };
    let id = receiver.header.map_or(receiver.assembling, |h| h.id);
    let value = Snapshot {
        progress: receiver.progress(id, Status::Ok),
        control_result,
        control_pending: CONTROL.load(Ordering::Acquire) != 0,
        release: receiver.policy.release,
    };
    let saved = port::portSET_INTERRUPT_MASK_FROM_ISR();
    core::ptr::addr_of_mut!(SNAPSHOT).write(value);
    port::portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
}
pub fn snapshot(_: &TaskContext) -> Snapshot {
    let saved = port::portSET_INTERRUPT_MASK_FROM_ISR();
    let mut value = unsafe { core::ptr::addr_of!(SNAPSHOT).read() };
    value.control_pending = CONTROL.load(Ordering::Acquire) != 0;
    port::portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
    value
}
fn request(command: u8) -> Result<(), Status> {
    CONTROL
        .compare_exchange(0, command, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Status::Busy)?;
    crate::kernel::radio::task::notify_work();
    Ok(())
}
/// Request selection of the verified candidate. Success acknowledges the request;
/// `snapshot` reports completion and its result after the peer task performs it.
/// This does not reset the chip; local activation policy owns that decision.
pub fn select_trial(_: &TaskContext) -> Result<(), Status> {
    request(1)
}
/// Request local boot confirmation; observe completion through `snapshot`.
pub fn confirm(_: &TaskContext) -> Result<(), Status> {
    request(2)
}
pub(crate) fn reset(_: &TaskContext) -> Result<(), Status> {
    request(3)
}
pub(crate) fn pending() -> bool {
    CONTROL.load(Ordering::Acquire) != 0
}

pub(crate) fn poll_control() {
    let command = CONTROL.load(Ordering::Acquire);
    if !matches!(command, 1 | 2 | 3) {
        return;
    }
    if CONTROL
        .compare_exchange(command, command | 0x80, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    if command == 3 {
        // Give the local USB task a chance to transmit its accepted-reset reply.
        let context = unsafe { TaskContext::assume() };
        crate::delay_ms(&context, 100);
        #[cfg(feature = "port-nrf52840")]
        cortex_m::peripheral::SCB::sys_reset();
        #[cfg(feature = "port-esp32c6")]
        esp_hal::system::software_reset();
    }
    let result = unsafe {
        match (&mut *core::ptr::addr_of_mut!(RECEIVER)).as_mut() {
            Some(r) => {
                if command == 1 {
                    r.activate()
                } else {
                    r.confirm()
                }
            }
            None => Err(Status::Unsupported),
        }
    };
    unsafe {
        publish(result.err().unwrap_or(Status::Ok));
    }
    CONTROL.store(0, Ordering::Release);
}
pub(crate) fn receive(request: Request<'_>) -> Progress {
    let reply = unsafe {
        match (&mut *core::ptr::addr_of_mut!(RECEIVER)).as_mut() {
            Some(r) => r.handle(request),
            None => Progress::unsupported(request.package()),
        }
    };
    let result = unsafe { core::ptr::addr_of!(SNAPSHOT).read().control_result };
    unsafe {
        publish(result);
    }
    reply
}
