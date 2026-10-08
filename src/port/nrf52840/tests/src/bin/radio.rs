//! Board entry for normal peer participation, with optional observers or RF probes.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]
#[cfg(any(feature = "radio-probe", feature = "peer-observer"))]
use core::ffi::c_void;
use lattice_vertex::{port, sync::TaskContext};
use rtt_target::rprintln as log;

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
    loop {
        lattice_vertex::delay_ms(&context, 1000);
        log!(
            "RADIO tick={} {:?}",
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
#[cortex_m_rt::entry]
fn main() -> ! {
    rtt_target::rtt_init_print!();
    let core = cortex_m::Peripherals::take().unwrap();
    let p = port::nrf52840::initialize(core.SYST);
    let radio = p.RADIO;
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
        let surface = if port::nrf52840::hal::pac::FICR.deviceid(0).read() & 1 != 0 {
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
