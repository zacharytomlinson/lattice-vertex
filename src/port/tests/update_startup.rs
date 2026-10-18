//! Maintained watchdog failures. No software reset, panic, or host command ends
//! these stalls: the hardware deadline and bootloader must restore the old image.
use lattice_vertex::{
    kernel::radio::testing::{self, Event},
    port,
};

fn stall() -> ! {
    port::portDISABLE_INTERRUPTS();
    loop {
        core::hint::spin_loop();
    }
}
pub fn before_init() {
    if cfg!(feature = "ota-stall-before-init") {
        stall();
    }
}
pub fn observe() {
    if cfg!(feature = "ota-stall-before-radio") {
        testing::observe(|event| {
            if matches!(event, Event::DiscoveryStarted { .. }) {
                stall();
            }
        });
    }
}
