#![no_std]
#![no_main]

use embassy_nrf::{nvmc::Nvmc, pac};
use lattice_nrf52840_boot::{layout::XIAO, prepare_boot, watchdog, BootState};

#[used]
#[link_section = ".lattice_layout"]
static LAYOUT: [u32; 16] = lattice_nrf52840_boot::layout::DESCRIPTOR;

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_nrf::init(Default::default());
    // A debugger/core-only handoff may retain a running watchdog. Give the
    // bounded swap its own interval, then a fresh interval to the trial.
    watchdog::feed();
    // Only application-region flash is passed to the swap algorithm. Factory
    // MBR, SoftDevice, storage, settings and USB recovery loader remain untouched.
    let (_, state) = prepare_boot(Nvmc::new(p.NVMC), XIAO).unwrap_or_else(|_| recovery());
    let vectors = unsafe { &*(XIAO.active_start as *const [u8; 8]) };
    if !XIAO.valid_vectors(XIAO.image_capacity(), vectors) {
        recovery();
    }
    // Arm after the full-slot swap, before any candidate instruction executes.
    if state == BootState::Swap {
        watchdog::start();
        watchdog::feed();
    }
    unsafe {
        cortex_m::interrupt::disable();
        let mut core = cortex_m::Peripherals::steal();
        core.SYST.disable_interrupt();
        core.SYST.disable_counter();
        core.SCB.icsr.write((1 << 25) | (1 << 27)); // clear SysTick/PendSV
        for n in 0..2 {
            core.NVIC.icer[n].write(u32::MAX);
            core.NVIC.icpr[n].write(u32::MAX);
        }
        core.SCB.vtor.write(XIAO.active_start);
        cortex_m::register::basepri::write(0);
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
        cortex_m::interrupt::enable();
        cortex_m::asm::bootload(XIAO.active_start as *const u32)
    }
}

fn recovery() -> ! {
    // Factory serial DFU entry, also used by the existing kernel USB fixture.
    pac::POWER.gpregret().write(|w| w.set_gpregret(0x4e));
    cortex_m::peripheral::SCB::sys_reset()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    recovery()
}
