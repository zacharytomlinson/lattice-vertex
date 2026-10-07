//! Temporary C6 RF investigation using the existing kernel radio workload.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]
use core::ffi::c_void;
use esp_println::println as log;
use lattice_vertex::{
    kernel, port,
    sync::{TaskContext, TaskHandle},
    types::StackType_t,
};
mod antenna_gpio;
#[cfg(feature = "rf-registers")]
mod radio_registers;
esp_bootloader_esp_idf::esp_app_desc!();
const _: () = assert!(lattice_vertex::configSUPPORT_DYNAMIC_ALLOCATION == 0);
static mut STACK: [StackType_t; 1024] = [0; 1024];
static mut CONTROL: kernel::tasks::StaticTask_t = kernel::tasks::StaticTask_t::new();
unsafe extern "C" {
    fn bt_bb_get_zb_tx_on_delay() -> u32;
}

extern "C" fn report(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    let mac = esp_hal::efuse::base_mac_address();
    let mut pins_reported = false;
    #[cfg(feature = "antenna-gpio-test")]
    let mut pins_restored = false;
    #[cfg(feature = "rf-registers")]
    let mut registers_reported = false;
    loop {
        lattice_vertex::delay_ms(&context, 1000);
        if !pins_reported {
            antenna_gpio::report("running");
            pins_reported = true;
        }
        #[cfg(feature = "antenna-gpio-test")]
        if !pins_restored && kernel::tasks::xTaskGetTickCount() >= 30_000 {
            antenna_gpio::restore();
            pins_restored = true;
        }
        #[cfg(feature = "rf-registers")]
        if !registers_reported {
            radio_registers::report();
            registers_reported = true;
        }
        // Observe hardware settings, not the requested configuration record.
        // These raw RF diagnostics belong to this temporary investigation image.
        let regs = esp_hal::peripherals::IEEE802154::regs();
        log!(
            "RF mac={} hop={} power_index={} bb_tx_on={} mac_tx_on={} rx_on={} txrx_switch={} crc={} sfd_timeout={} filter={} no_rss={}",
            mac,
            regs.channel().read().hop().bits(),
            regs.tx_power().read().tx_power().bits(),
            unsafe { bt_bb_get_zb_tx_on_delay() },
            regs.txon_delay().read().txon_delay().bits(),
            regs.rxon_delay().read().rxon_delay().bits(),
            regs.txrx_switch_delay().read().bits() & 0x3ff,
            regs.crc_error_cnt().read().bits(),
            regs.sfd_timeout_cnt().read().bits(),
            regs.rx_filter_fail_cnt().read().bits(),
            regs.no_rss_detect_cnt().read().bits(),
        );
        log!(
            "RADIO mac={} tick={} {:?}",
            mac,
            kernel::tasks::xTaskGetTickCount(),
            lattice_radio_test::statistics()
        );
    }
}
#[esp_hal::main]
fn main() -> ! {
    let p =
        esp_hal::init(esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max()));
    antenna_gpio::report("startup");
    #[cfg(feature = "antenna-gpio-test")]
    let _antenna_outputs = antenna_gpio::drive_low(p.GPIO3, p.GPIO14);
    let timer = esp_hal::timer::systimer::SystemTimer::new(p.SYSTIMER);
    let irq = esp_hal::interrupt::software::SoftwareInterruptControl::new(p.SW_INTERRUPT);
    port::esp32c6::initialize(timer.alarm0, irq.software_interrupt0);
    let context = unsafe { TaskContext::assume() };
    unsafe {
        TaskHandle::spawn_static(&context, b"report", &mut STACK, &mut CONTROL, 1, report).unwrap();
    }
    log!("START C6 RF investigation: {}", port::portARCH_NAME);
    lattice_radio_test::start(context, p.IEEE802154)
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    port::portDISABLE_INTERRUPTS();
    log!("FAIL {}", info);
    loop {
        core::hint::spin_loop();
    }
}
