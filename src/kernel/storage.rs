//! One physical flash owner for program configuration and optional signed updates.
//! Access after startup belongs exclusively to the resident peer task.

pub(crate) mod startup;

#[cfg(any(feature = "nrf52840-update", feature = "esp32c6-update"))]
mod device {
    #[cfg(feature = "nrf52840-update")]
    pub type PlatformStorage = crate::port::nrf52840::update::ImageStorage<'static>;
    #[cfg(feature = "esp32c6-update")]
    pub type PlatformStorage = crate::port::esp32c6::update::ImageStorage<'static>;

    enum Owner {
        Empty,
        Local(PlatformStorage),
        #[cfg(feature = "firmware-update")]
        Updates(crate::kernel::update::Receiver<PlatformStorage>),
    }
    static mut OWNER: Owner = Owner::Empty;
    static mut STARTUP: super::startup::Startup = super::startup::Startup::new(0);

    pub(crate) fn initialize(storage: PlatformStorage) {
        assert_eq!(
            crate::kernel::tasks::xTaskGetSchedulerState(),
            crate::kernel::tasks::taskSCHEDULER_NOT_STARTED
        );
        unsafe {
            assert!(
                matches!(&*core::ptr::addr_of!(OWNER), Owner::Empty),
                "flash already owned"
            );
            core::ptr::addr_of_mut!(OWNER).write(Owner::Local(storage));
            core::ptr::addr_of_mut!(STARTUP).write(super::startup::Startup::new(
                crate::kernel::tasks::xTaskGetTickCount() as u32,
            ));
        }
    }
    #[cfg(feature = "firmware-update")]
    pub(crate) fn enable_updates(policy: crate::kernel::update::package::Policy) {
        assert_eq!(
            crate::kernel::tasks::xTaskGetSchedulerState(),
            crate::kernel::tasks::taskSCHEDULER_NOT_STARTED
        );
        unsafe {
            let owner = &mut *core::ptr::addr_of_mut!(OWNER);
            let Owner::Local(storage) = core::mem::replace(owner, Owner::Empty) else {
                panic!("initialize flash before updates")
            };
            *owner = Owner::Updates(crate::kernel::update::Receiver::new(storage, policy));
        }
    }
    /// Startup, then the one resident peer task. The borrow cannot escape.
    pub(crate) fn with_storage<R>(operation: impl FnOnce(&mut PlatformStorage) -> R) -> Option<R> {
        unsafe {
            match &mut *core::ptr::addr_of_mut!(OWNER) {
                Owner::Empty => None,
                Owner::Local(storage) => Some(operation(storage)),
                #[cfg(feature = "firmware-update")]
                Owner::Updates(receiver) => Some(operation(&mut receiver.storage)),
            }
        }
    }
    /// Caller is the resident updater; no storage borrow may overlap this one.
    #[cfg(feature = "firmware-update")]
    pub(crate) unsafe fn receiver(
    ) -> Option<&'static mut crate::kernel::update::Receiver<PlatformStorage>> {
        match &mut *core::ptr::addr_of_mut!(OWNER) {
            Owner::Updates(r) => Some(r),
            _ => None,
        }
    }
    pub(crate) fn startup_progress(radio_completed: bool) -> bool {
        // Signed-update startup owns its existing trial/confirmation path.
        unsafe {
            if let Owner::Local(storage) = &mut *core::ptr::addr_of_mut!(OWNER) {
                if is_trial(storage) && !crate::kernel::program::runtime::reconstructed() {
                    return true;
                }
                let startup = &mut *core::ptr::addr_of_mut!(STARTUP);
                if startup.progress(
                    crate::kernel::tasks::xTaskGetTickCount() as u32,
                    radio_completed,
                    || {
                        storage
                            .confirm()
                            .map_err(|_| lattice_peer_wire::update::Status::Storage)
                    },
                ) {
                    crate::port::watchdog::feed();
                }
                return true;
            }
        }
        false
    }
    pub(crate) fn is_trial(storage: &PlatformStorage) -> bool {
        #[cfg(feature = "nrf52840-update")]
        {
            matches!(
                storage.boot_state(),
                crate::port::nrf52840::update::BootState::Swap
            )
        }
        #[cfg(feature = "esp32c6-update")]
        {
            storage.boot_state() == crate::port::esp32c6::update::BootState::PendingVerify
        }
    }
}
#[cfg(any(feature = "nrf52840-update", feature = "esp32c6-update"))]
pub use device::PlatformStorage;
#[cfg(all(
    feature = "firmware-update",
    any(feature = "nrf52840-update", feature = "esp32c6-update")
))]
pub(crate) use device::{enable_updates, is_trial, receiver};
#[cfg(any(feature = "nrf52840-update", feature = "esp32c6-update"))]
pub(crate) use device::{initialize, startup_progress, with_storage};
