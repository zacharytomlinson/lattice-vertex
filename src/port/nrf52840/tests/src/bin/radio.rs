//! Board entry for normal peer participation, with optional observers or RF probes.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]
#[cfg(any(
    feature = "radio-probe",
    feature = "peer-observer",
    feature = "usb-console"
))]
use core::ffi::c_void;
use lattice_vertex::{port, sync::TaskContext};
use rtt_target::rprintln as log;

#[cfg(feature = "usb-console")]
#[path = "../usb_console.rs"]
mod usb_console;

#[cfg(any(
    feature = "radio-probe",
    feature = "peer-observer",
    feature = "usb-console"
))]
use lattice_vertex::{kernel, sync::TaskHandle, types::StackType_t};
const _: () = assert!(lattice_vertex::configSUPPORT_DYNAMIC_ALLOCATION == 0);
#[cfg(all(
    not(feature = "usb-console"),
    any(feature = "radio-probe", feature = "peer-observer")
))]
static mut STACK: [StackType_t; 1024] = [0; 1024];
#[cfg(feature = "usb-console")]
static mut STACK: [StackType_t; 2048] = [0; 2048];
#[cfg(any(
    feature = "radio-probe",
    feature = "peer-observer",
    feature = "usb-console"
))]
static mut CONTROL: kernel::tasks::StaticTask_t = kernel::tasks::StaticTask_t::new();
#[cfg(all(
    feature = "radio-probe",
    any(feature = "peer-observer", feature = "test-appliance")
))]
compile_error!("select either the RF probe workload or the normal discovery observer");

#[cfg(any(
    feature = "radio-probe",
    feature = "peer-observer",
    feature = "usb-console"
))]
fn report_once(context: &TaskContext, out: &mut dyn core::fmt::Write) -> core::fmt::Result {
    let _ = context;
    #[cfg(feature = "radio-probe")]
    {
        writeln!(
            out,
            "RADIO tick={} {:?}",
            kernel::tasks::xTaskGetTickCount(),
            lattice_radio_test::statistics()
        )?;
    }
    #[cfg(all(feature = "peer-observer", not(feature = "radio-probe")))]
    {
        let stats = lattice_radio_test::discovery::statistics(context);
        writeln!(
            out,
            "DISCOVERY tick={} local={:016x} peer={:016x} {:?}",
            kernel::tasks::xTaskGetTickCount(),
            stats.local,
            stats.peer,
            stats
        )?;
        writeln!(
            out,
            "SURFACE tick={} {:?}",
            kernel::tasks::xTaskGetTickCount(),
            lattice_radio_test::surface::statistics(context)
        )?;
        if let Some(part) = lattice_radio_test::surface::take_part(context) {
            writeln!(out, "SURFACE_PART {:?}", part)?;
        }
    }
    #[cfg(not(any(feature = "radio-probe", feature = "peer-observer")))]
    {
        writeln!(out, "KERNEL tick={}", kernel::tasks::xTaskGetTickCount())?;
    }
    Ok(())
}

#[cfg(any(
    feature = "radio-probe",
    feature = "peer-observer",
    feature = "usb-console"
))]
extern "C" fn report(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    #[cfg(feature = "usb-console")]
    usb_console::run(&context);
    #[cfg(not(feature = "usb-console"))]
    {
        struct Rtt;
        impl core::fmt::Write for Rtt {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                rtt_target::rprint!("{}", s);
                Ok(())
            }
        }
        loop {
            lattice_vertex::delay_ms(&context, 1000);
            report_once(&context, &mut Rtt).unwrap();
        }
    }
}
#[cortex_m_rt::entry]
fn main() -> ! {
    rtt_target::rtt_init_print!();
    let core = cortex_m::Peripherals::take().unwrap();
    let p = port::nrf52840::initialize(core.SYST);
    let radio = p.RADIO;
    let context = unsafe { TaskContext::assume() };
    #[cfg(feature = "usb-console")]
    usb_console::initialize(p.USBD);
    #[cfg(any(
        feature = "radio-probe",
        feature = "peer-observer",
        feature = "usb-console"
    ))]
    unsafe {
        TaskHandle::spawn_static(&context, b"report", &mut STACK, &mut CONTROL, 1, report).unwrap();
    }
    #[cfg(feature = "peer-observer")]
    lattice_radio_test::discovery::observe();
    log!("START kernel radio: {}", port::portARCH_NAME);
    #[cfg(feature = "radio-probe")]
    lattice_radio_test::start(context, radio);
    #[cfg(any(feature = "peer-observer", feature = "test-appliance"))]
    {
        let appliance = lattice_radio_test::surface::appliance(
            &context,
            cfg!(feature = "test-appliance")
                || port::nrf52840::hal::pac::FICR.deviceid(0).read() & 1 != 0,
        );
        lattice_vertex::kernel::start(context, radio, appliance)
    }
    #[cfg(not(any(
        feature = "radio-probe",
        feature = "peer-observer",
        feature = "test-appliance"
    )))]
    {
        static APPLIANCE: lattice_vertex::kernel::Appliance<(), core::convert::Infallible> =
            lattice_vertex::kernel::Appliance::new(&lattice_vertex::surface::EMPTY);
        APPLIANCE.initialize(&context, ());
        lattice_vertex::kernel::start(context, radio, &APPLIANCE)
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    port::portDISABLE_INTERRUPTS();
    log!("FAIL {}", info);
    loop {
        core::hint::spin_loop();
    }
}
