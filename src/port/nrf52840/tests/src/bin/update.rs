//! Local flash/boot check through USB, with the ordinary kernel and empty surface.
//! Commands are fixture-only; this is not the Lattice OTA wire protocol.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::ffi::c_void;
use lattice_vertex::{
    kernel, port,
    sync::{TaskContext, TaskHandle},
    types::StackType_t,
};
use port::nrf52840::{
    hal,
    update::{self, layout::XIAO, Error, ImageStorage},
};

#[path = "../usb_console.rs"]
mod usb_console;

#[path = "../../../../tests/update_commands.rs"]
mod update_commands;
#[path = "../../../../tests/update_measurement.rs"]
mod update_measurement;
const _: () = assert!(lattice_vertex::config::configTICK_RATE_HZ == 1000);

static mut STACK: [StackType_t; 3072] = [0; 3072];
static mut CONTROL: kernel::tasks::StaticTask_t = kernel::tasks::StaticTask_t::new();
static mut STORAGE: Option<ImageStorage<'static>> = None;
static mut DECODER: update_commands::Decoder = update_commands::Decoder::new();
static mut SEQUENCE: u32 = 0;
static mut OPCODE: u8 = 0;
static mut RESULT: Result<(), Error> = Ok(());
static mut PROGRESS: u32 = 0;
static mut PENDING: bool = false;
static mut RESET: u8 = 0;
static mut REPORT_TICK: u32 = 0;
static mut ELAPSED_US: u32 = 0;

fn factory_ready() -> bool {
    // Read only. Factory settings are never rewritten by the test or Lattice loader.
    let settings = unsafe { &*(0xff000 as *const [u8; 12]) };
    update::layout::xiao_factory_crc_allows_updates(settings)
}

fn receive(bytes: &[u8]) {
    // USB is a byte stream: a record may be split or coalesced across packets.
    for &byte in bytes {
        unsafe {
            if let Some(body) = DECODER.push(byte, kernel::tasks::xTaskGetTickCount()) {
                command(body);
            }
        }
    }
}

fn command(bytes: &[u8]) {
    unsafe {
        SEQUENCE = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        OPCODE = bytes[4];
        let payload = &bytes[5..];
        let storage = STORAGE.as_mut().unwrap();
        let started = cortex_m::peripheral::DWT::cycle_count();
        RESULT = match (OPCODE, payload.len()) {
            (0, 0) => Ok(()),
            (1, 4) if factory_ready() => {
                PROGRESS = 0;
                storage.begin(u32::from_le_bytes(payload.try_into().unwrap()))
            }
            (2, 0) if factory_ready() => storage
                .erase_next_page()
                .map(|ready| PROGRESS = ready as u32),
            (3, 5..=59) if factory_ready() => {
                let offset = u32::from_le_bytes(payload[..4].try_into().unwrap());
                storage
                    .write(offset, &payload[4..])
                    .map(|()| PROGRESS = offset + payload.len() as u32 - 4)
            }
            (4, 32) if factory_ready() => storage.select_trial(payload.try_into().unwrap()),
            (5, 0) => {
                RESET = 1;
                Ok(())
            }
            (6, 0) if factory_ready() => storage.confirm(),
            (7, 0) => {
                RESET = 2;
                Ok(())
            }
            (8, 32) if factory_ready() => storage.verify_image(payload.try_into().unwrap()),
            _ => Err(Error::Order),
        };
        ELAPSED_US = cortex_m::peripheral::DWT::cycle_count()
            .wrapping_sub(started)
            .div_ceil(64);
        PENDING = true;
    }
}

fn report_ready() -> bool {
    unsafe {
        if RESET != 0
            && !PENDING
            && kernel::tasks::xTaskGetTickCount().wrapping_sub(REPORT_TICK) >= 100
        {
            if RESET == 2 {
                hal::pac::POWER.gpregret().write(|w| w.set_gpregret(0x4e));
            }
            cortex_m::peripheral::SCB::sys_reset();
        }
        PENDING
    }
}

fn report_once(_: &TaskContext, out: &mut dyn core::fmt::Write) -> core::fmt::Result {
    unsafe {
        writeln!(
            out,
            "UPDATE image={} boot={:?} ready={} seq={} op={} result={:?} progress={} tick={} elapsed_us={} radio_free={} peer_ready={}",
            if cfg!(feature = "update-image-b") {
                "B"
            } else {
                "A"
            },
            STORAGE.as_ref().unwrap().boot_state(),
            factory_ready() as u8,
            SEQUENCE,
            OPCODE,
            RESULT,
            PROGRESS,
            kernel::tasks::xTaskGetTickCount(), ELAPSED_US, update_measurement::radio_free_bytes(), update_measurement::peer_ready() as u8
        )?;
        PENDING = false;
        REPORT_TICK = kernel::tasks::xTaskGetTickCount();
        Ok(())
    }
}

extern "C" fn console(_: *mut c_void) {
    usb_console::run_with_commands(&unsafe { TaskContext::assume() }, receive, report_ready)
}

#[cortex_m_rt::entry]
fn main() -> ! {
    extern "C" fn stack_overflow(_: lattice_vertex::types::TaskHandle_t, _: *const u8) {
        panic!("local update task stack overflow");
    }
    kernel::tasks::vTaskSetStackOverflowHook(Some(stack_overflow));
    update_measurement::start();
    rtt_target::rtt_init_print!();
    let mut core = cortex_m::Peripherals::take().unwrap();
    core.DCB.enable_trace();
    core.DWT.enable_cycle_counter();
    let p = port::nrf52840::initialize(core.SYST);
    let context = unsafe { TaskContext::assume() };
    usb_console::initialize(p.USBD);
    unsafe {
        STORAGE = Some(update::image_storage(p.NVMC, XIAO).unwrap());
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
    kernel::start(context, p.RADIO, &APPLIANCE)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    rtt_target::rprintln!("FAIL update fixture: {}", info);
    // Recoverable without depending on a working application USB stack.
    hal::pac::POWER.gpregret().write(|w| w.set_gpregret(0x4e));
    cortex_m::peripheral::SCB::sys_reset()
}
