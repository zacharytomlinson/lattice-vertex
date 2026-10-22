//! Maintained OTA check: normal kernel receiver and binary native-USB host link.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]
use core::ffi::c_void;
use esp_hal::{usb_serial_jtag::UsbSerialJtag, Blocking};
use lattice_vertex::{
    kernel::{self, host},
    port,
    sync::{TaskContext, TaskHandle},
    types::StackType_t,
};
esp_bootloader_esp_idf::esp_app_desc!();
#[path = "../../../../tests/update_policy.rs"]
mod update_policy;
#[path = "../../../../tests/update_startup.rs"]
mod update_startup;
static mut USB: Option<UsbSerialJtag<'static, Blocking>> = None;
static mut STACK: [StackType_t; 2048] = [0; 2048];
static mut CONTROL: kernel::tasks::StaticTask_t = kernel::tasks::StaticTask_t::new();

extern "C" fn stack_overflow(_: lattice_vertex::types::TaskHandle_t, _: *const u8) {
    panic!("OTA task stack overflow");
}

#[cfg(feature = "ota-activation-check")]
#[path = "../../../../tests/update_activation.rs"]
mod update_activation;
#[cfg(feature = "ota-staging-check")]
#[path = "../../../../tests/update_staging.rs"]
mod update_staging;
extern "C" fn usb_task(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    #[cfg(feature = "ota-activation-check")]
    let mut activation_check = update_activation::Check::new(&context);
    #[cfg(feature = "ota-staging-check")]
    let mut staging_check = update_staging::Check::new();
    let mut usb = unsafe { USB.take().unwrap() };
    let mut decoder = host::Decoder::new();
    let mut output = [0; host::MAX_PACKET + 1];
    host::connect(&context);
    loop {
        #[cfg(feature = "ota-activation-check")]
        activation_check.poll(&context);
        #[cfg(feature = "ota-staging-check")]
        staging_check.poll(&context);
        let mut rejected = None;
        while let Ok(byte) = usb.read_byte() {
            if let Some(request) = decoder.push(byte) {
                if let Err(failure) = host::submit(&context, request) {
                    rejected = Some(host::Packet::Error {
                        token: request.token(),
                        failure,
                    });
                    break;
                }
            }
        }
        if let Some(packet) = rejected.or_else(|| host::receive(&context)) {
            let n = packet.encode((&mut output[1..]).try_into().unwrap()) + 1;
            // Delimiters recover from boot text and interrupted records.
            for &byte in &output[..n] {
                while usb.write_byte_nb(byte).is_err() {
                    lattice_vertex::delay_ms(&context, 1);
                }
            }
            let _ = usb.flush_tx();
        }
        lattice_vertex::delay_ms(&context, 1);
    }
}
#[esp_hal::main]
fn main() -> ! {
    update_startup::before_init();
    update_startup::observe();
    kernel::tasks::vTaskSetStackOverflowHook(Some(stack_overflow));
    let p =
        esp_hal::init(esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max()));
    let timer = esp_hal::timer::systimer::SystemTimer::new(p.SYSTIMER);
    let irq = esp_hal::interrupt::software::SoftwareInterruptControl::new(p.SW_INTERRUPT);
    port::esp32c6::initialize(timer.alarm0, irq.software_interrupt0);
    let context = unsafe { TaskContext::assume() };
    let storage = port::esp32c6::update::image_storage(p.FLASH).unwrap();
    unsafe {
        USB = Some(UsbSerialJtag::new(p.USB_DEVICE));
        TaskHandle::spawn_static(&context, b"ota-usb", &mut STACK, &mut CONTROL, 1, usb_task)
            .unwrap();
    }
    #[cfg(feature = "ota-appliance")]
    let appliance = lattice_radio_test::surface::appliance(&context, true);
    #[cfg(not(feature = "ota-appliance"))]
    let appliance = {
        static APPLIANCE: kernel::Appliance<(), core::convert::Infallible> =
            kernel::Appliance::new(&lattice_vertex::surface::EMPTY);
        APPLIANCE.initialize(&context, ());
        &APPLIANCE
    };
    kernel::start_with_updates(
        context,
        p.IEEE802154,
        appliance,
        storage,
        update_policy::policy(2, kernel::update::appliance_surface_identity(appliance)),
    )
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("FAIL OTA fixture: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
