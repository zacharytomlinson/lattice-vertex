//! Board entry for normal peer participation, with optional observers or RF probes.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]
#[cfg(any(feature = "radio-probe", feature = "peer-observer"))]
use core::ffi::c_void;
use esp_println::println as log;
use lattice_vertex::{port, sync::TaskContext};
esp_bootloader_esp_idf::esp_app_desc!();
#[cfg(any(feature = "radio-probe", feature = "peer-observer"))]
use lattice_vertex::{kernel, sync::TaskHandle, types::StackType_t};
const _: () = assert!(lattice_vertex::configSUPPORT_DYNAMIC_ALLOCATION == 0);
#[cfg(any(feature = "radio-probe", feature = "peer-observer"))]
static mut STACK: [StackType_t; 1024] = [0; 1024];
#[cfg(any(feature = "radio-probe", feature = "peer-observer"))]
static mut CONTROL: kernel::tasks::StaticTask_t = kernel::tasks::StaticTask_t::new();

#[cfg(all(feature = "radio-probe", feature = "peer-observer"))]
compile_error!("select either the RF probe workload or the normal discovery observer");

#[cfg(feature = "radio-probe")]
extern "C" fn report(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    let mac = esp_hal::efuse::base_mac_address();
    loop {
        lattice_vertex::delay_ms(&context, 1000);
        log!(
            "RADIO mac={} tick={} {:?}",
            mac,
            kernel::tasks::xTaskGetTickCount(),
            lattice_radio_test::statistics()
        );
    }
}
#[cfg(all(feature = "peer-observer", not(feature = "radio-probe")))]
extern "C" fn report(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    loop {
        lattice_vertex::delay_ms(&context, 1000);
        let stats = lattice_radio_test::discovery::statistics(&context);
        log!(
            "DISCOVERY tick={} local={:016x} peer={:016x} {:?}",
            kernel::tasks::xTaskGetTickCount(),
            stats.local,
            stats.peer,
            stats
        );
        log!(
            "SURFACE tick={} {:?}",
            kernel::tasks::xTaskGetTickCount(),
            lattice_radio_test::surface::statistics(&context)
        );
    }
}
#[esp_hal::main]
fn main() -> ! {
    // The kernel tick comes from SYSTIMER, independently of CPU division.
    let p =
        esp_hal::init(esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max()));
    let timer = esp_hal::timer::systimer::SystemTimer::new(p.SYSTIMER);
    let irq = esp_hal::interrupt::software::SoftwareInterruptControl::new(p.SW_INTERRUPT);
    port::esp32c6::initialize(timer.alarm0, irq.software_interrupt0);
    let radio = p.IEEE802154;
    let context = unsafe { TaskContext::assume() };
    #[cfg(any(feature = "radio-probe", feature = "peer-observer"))]
    unsafe {
        TaskHandle::spawn_static(&context, b"report", &mut STACK, &mut CONTROL, 1, report).unwrap();
    }
    #[cfg(feature = "peer-observer")]
    lattice_radio_test::discovery::observe();
    log!("START kernel radio: {}", port::portARCH_NAME);
    #[cfg(feature = "radio-probe")]
    lattice_radio_test::start(context, radio);
    #[cfg(feature = "peer-observer")]
    {
        let surface = if esp_hal::efuse::base_mac_address().as_bytes()[5] & 0x40 != 0 {
            &lattice_radio_test::surface::DIMMER
        } else {
            &lattice_radio_test::surface::SWITCH
        };
        lattice_vertex::kernel::start(context, radio, surface)
    }
    #[cfg(not(any(feature = "radio-probe", feature = "peer-observer")))]
    lattice_vertex::kernel::start(context, radio, &lattice_vertex::surface::EMPTY)
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    port::portDISABLE_INTERRUPTS();
    log!("FAIL {}", info);
    loop {
        core::hint::spin_loop();
    }
}
