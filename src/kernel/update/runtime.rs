//! The resident peer task owns flash, the receiver, and activation decisions.
//! Physical-work holds share only atomic admission state. Local maintenance
//! requests use one notification, not an appliance-owned networking loop.
use super::{
    activation::{self, Action, Activation},
    package::Policy,
    Receiver, Storage,
};
use crate::{kernel::tasks, port, sync::TaskContext};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
    fn verify_step(&mut self, d: &[u8; 32], bytes: u32) -> Result<bool, Self::Error> {
        PlatformStorage::verify_step(self, d, bytes)
    }
    fn select_trial(&mut self, d: &[u8; 32]) -> Result<(), Self::Error> {
        PlatformStorage::select_trial(self, d)
    }
    fn confirm(&mut self) -> Result<(), Self::Error> {
        PlatformStorage::confirm(self)
    }
    fn activation_swap_bytes(&self) -> u32 {
        #[cfg(feature = "port-nrf52840")]
        {
            self.image_capacity()
        }
        #[cfg(feature = "port-esp32c6")]
        {
            0
        }
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
static mut ACTIVATION: Activation = Activation::new();
static mut ACTIVITY_EPOCH: u32 = 0;
static mut STAGING: super::staging::Staging = super::staging::Staging::new();
static mut STARTUP: super::startup::Startup = super::startup::Startup::new(0);
// Other tasks read this monotonic result without borrowing the peer-owned record.
static STARTUP_CONFIRMED: AtomicBool = AtomicBool::new(false);
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
        core::ptr::addr_of_mut!(STARTUP).write(super::startup::Startup::new(
            tasks::xTaskGetTickCount() as u32,
        ));
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
    // Application code cannot bypass the kernel's startup checks.
    if !STARTUP_CONFIRMED.load(Ordering::Acquire) {
        return Err(Status::Busy);
    }
    request(2)
}

/// Only the resident task calls this. A successful local radio operation plus
/// tick progress proves startup without depending on any other vertex.
pub(crate) fn startup_progress(radio_completed: bool) {
    let Some(receiver) = (unsafe { (&mut *core::ptr::addr_of_mut!(RECEIVER)).as_mut() }) else {
        return;
    };
    let startup = unsafe { &mut *core::ptr::addr_of_mut!(STARTUP) };
    if startup.progress(tasks::xTaskGetTickCount() as u32, radio_completed, || {
        receiver.confirm()
    }) {
        STARTUP_CONFIRMED.store(true, Ordering::Release);
        port::watchdog::feed();
    }
}
pub(crate) fn reset(_: &TaskContext) -> Result<(), Status> {
    request(3)
}
pub(crate) fn pending() -> bool {
    CONTROL.load(Ordering::Acquire) != 0
}

pub(crate) fn contending() -> bool {
    unsafe { (&*core::ptr::addr_of!(ACTIVATION)).contending() }
}
pub(crate) fn awaiting_activation() -> bool {
    unsafe {
        (&*core::ptr::addr_of!(RECEIVER)).as_ref().is_some_and(|r| {
            r.phase == lattice_peer_wire::update::Phase::Ready
                && r.header.is_some_and(|h| {
                    h.costs
                        .outage_ms(r.storage.activation_swap_bytes())
                        .is_some()
                })
        })
    }
}
/// Called only by the resident peer, after ordinary local/received work. Sending
/// a notice gives neighbors a receive turn before final admission is closed.
pub(crate) fn poll_activation(
    now: u32,
    local: u64,
    neighbor_busy: bool,
    work_pending: bool,
    mut send: impl FnMut(lattice_peer_wire::update::Absence) -> bool,
) {
    use lattice_peer_wire::update::{AbsencePhase, Phase};
    let state = unsafe { &mut *core::ptr::addr_of_mut!(ACTIVATION) };
    let epoch = activation::epoch();
    unsafe {
        if epoch != ACTIVITY_EPOCH {
            ACTIVITY_EPOCH = epoch;
            state.activity(now);
        }
    }
    let ready = unsafe {
        (&*core::ptr::addr_of!(RECEIVER)).as_ref().and_then(|r| {
            (r.phase == Phase::Ready)
                .then(|| (r.header.unwrap(), r.storage.activation_swap_bytes()))
        })
    };
    let busy = activation::held() || pending() || work_pending || crate::kernel::change::pending();
    match state.poll(now, local, ready, busy, neighbor_busy) {
        Action::Wait => {}
        Action::Announce(notice) => {
            let sent = send(notice);
            state.announced(now, sent);
        }
        Action::Cancel(notice) => {
            send(notice);
        }
        Action::Activate(notice) => {
            if !send(notice) {
                state.activity(now);
                return;
            }
            let commit = activation::Commit::enter();
            // An appliance task may have run during the radio send. Recheck
            // its activity and unsent deltas after closing new physical work.
            if commit.is_none()
                || activation::epoch() != epoch
                || crate::kernel::change::pending()
                || pending()
            {
                drop(commit);
                state.activity(now);
                send(lattice_peer_wire::update::Absence {
                    phase: AbsencePhase::Cancel,
                    duration_ms: 0,
                    ..notice
                });
                return;
            }
            let result = unsafe {
                (&mut *core::ptr::addr_of_mut!(RECEIVER))
                    .as_mut()
                    .unwrap()
                    .activate()
            };
            unsafe {
                publish(result.err().unwrap_or(Status::Ok));
            }
            if result.is_ok() {
                reset_chip();
            }
            drop(commit);
            state.activity(now);
            send(lattice_peer_wire::update::Absence {
                phase: AbsencePhase::Cancel,
                duration_ms: 0,
                ..notice
            });
        }
    }
}

fn reset_chip() -> ! {
    #[cfg(feature = "port-nrf52840")]
    cortex_m::peripheral::SCB::sys_reset();
    #[cfg(feature = "port-esp32c6")]
    esp_hal::system::software_reset();
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
        reset_chip();
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
pub(crate) fn receive(request: Request<'_>, traffic_pending: bool) -> Progress {
    let reply = unsafe {
        match (&mut *core::ptr::addr_of_mut!(RECEIVER)).as_mut() {
            Some(r) => {
                if let Some((work, costs)) = r.staging_work(request) {
                    let pacing = &mut *core::ptr::addr_of_mut!(STAGING);
                    let now = ((tasks::xTaskGetTickCount() as u64 * 1000)
                        / crate::configTICK_RATE_HZ as u64) as u32;
                    let busy = traffic_pending || crate::kernel::change::pending() || pending();
                    if !pacing.admit(now, activation::epoch(), busy, work, costs) {
                        return r.progress(request.package(), Status::Busy);
                    }
                    let Some(_flash) = super::staging::FlashStep::enter() else {
                        return r.progress(request.package(), Status::Busy);
                    };
                    let reply = r.handle(request);
                    let completed = ((tasks::xTaskGetTickCount() as u64 * 1000)
                        / crate::configTICK_RATE_HZ as u64)
                        as u32;
                    pacing.completed(completed, work, costs);
                    reply
                } else {
                    r.handle(request)
                }
            }
            None => Progress::unsupported(request.package()),
        }
    };
    let result = unsafe { core::ptr::addr_of!(SNAPSHOT).read().control_result };
    unsafe {
        publish(result);
    }
    reply
}
