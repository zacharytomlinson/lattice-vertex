//! Local boot/storage fixture; USB commands are not Lattice OTA messages.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use esp_hal::{usb_serial_jtag::UsbSerialJtag, Blocking};
use esp_println::println as log;
use lattice_vertex::{
    kernel, port,
    sync::{TaskContext, TaskHandle},
    types::StackType_t,
};
use port::esp32c6::update::{self, Error, ImageStorage};
esp_bootloader_esp_idf::esp_app_desc!();

#[path = "../../../../tests/update_commands.rs"]
mod update_commands;
const _: () = assert!(lattice_vertex::config::configTICK_RATE_HZ == 1000);

static mut STACK: [StackType_t; 3072] = [0; 3072];
static mut CONTROL: kernel::tasks::StaticTask_t = kernel::tasks::StaticTask_t::new();
static mut STORAGE: Option<ImageStorage<'static>> = None;
static mut USB: Option<UsbSerialJtag<'static, Blocking>> = None;

extern "C" fn console(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    let storage = unsafe { STORAGE.as_mut().unwrap() };
    let mut usb = unsafe { USB.take().unwrap() };
    let mut decoder = update_commands::Decoder::new();
    let mut last = 0;
    let mut progress = 0;
    loop {
        while let Ok(byte) = usb.read_byte() {
            if let Some(bytes) = decoder.push(byte, kernel::tasks::xTaskGetTickCount()) {
                let seq = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                let op = bytes[4];
                let payload = &bytes[5..];
                let result = match (op, payload.len()) {
                    (0, 0) => Ok(()),
                    (1, 4) => {
                        progress = 0;
                        storage.begin(u32::from_le_bytes(payload.try_into().unwrap()))
                    }
                    (2, 0) => storage
                        .erase_next_page()
                        .map(|ready| progress = ready as u32),
                    (3, 5..=59) => {
                        let offset = u32::from_le_bytes(payload[..4].try_into().unwrap());
                        storage
                            .write(offset, &payload[4..])
                            .map(|()| progress = offset + payload.len() as u32 - 4)
                    }
                    (4, 32) => storage.select_trial(payload.try_into().unwrap()),
                    (5, 0) => Ok(()),
                    (6, 0) => storage.confirm(),
                    _ => Err(Error::Order),
                };
                report(storage, seq, op, result, progress);
                if op == 5 && result.is_ok() {
                    lattice_vertex::delay_ms(&context, 100);
                    esp_hal::system::software_reset();
                }
                last = kernel::tasks::xTaskGetTickCount();
            }
        }
        let now = kernel::tasks::xTaskGetTickCount();
        if now.wrapping_sub(last) >= 1000 {
            report(storage, 0, 0, Ok(()), progress);
            last = now;
        }
        lattice_vertex::delay_ms(&context, 1);
    }
}

fn report(storage: &ImageStorage<'_>, seq: u32, op: u8, result: Result<(), Error>, progress: u32) {
    log!(
        "UPDATE image={} slot={} boot={:?} ready=1 seq={} op={} result={:?} progress={} tick={}",
        if cfg!(feature = "update-image-b") {
            "B"
        } else {
            "A"
        },
        storage.running_slot(),
        storage.boot_state(),
        seq,
        op,
        result,
        progress,
        kernel::tasks::xTaskGetTickCount()
    );
}

#[esp_hal::main]
fn main() -> ! {
    let p =
        esp_hal::init(esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max()));
    let timer = esp_hal::timer::systimer::SystemTimer::new(p.SYSTIMER);
    let irq = esp_hal::interrupt::software::SoftwareInterruptControl::new(p.SW_INTERRUPT);
    port::esp32c6::initialize(timer.alarm0, irq.software_interrupt0);
    let context = unsafe { TaskContext::assume() };
    unsafe {
        STORAGE = Some(update::image_storage(p.FLASH).unwrap());
        USB = Some(UsbSerialJtag::new(p.USB_DEVICE));
        TaskHandle::spawn_static(
            &context,
            b"update-check",
            &mut STACK,
            &mut CONTROL,
            1,
            console,
        )
        .unwrap();
    }
    static APPLIANCE: kernel::Appliance<(), core::convert::Infallible> =
        kernel::Appliance::new(&lattice_vertex::surface::EMPTY);
    APPLIANCE.initialize(&context, ());
    kernel::start(context, p.IEEE802154, &APPLIANCE)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    log!("FAIL update fixture: {}", info);
    // Keep diagnostics visible. Explicit reset/ROM download remains available;
    // automatic watchdog expiry is not part of this local boot check yet.
    loop {
        core::hint::spin_loop();
    }
}
