//! Hardware qualification of the kernel port, not appliance networking firmware.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use esp_hal::{
    interrupt::software::{SoftwareInterrupt, SoftwareInterruptControl},
    timer::systimer::SystemTimer,
};
use esp_println::println;
use lattice_vertex::{
    kernel::tasks::{self, StaticTask_t},
    port,
    sync::{TaskContext, TaskHandle},
    types::*,
};

esp_bootloader_esp_idf::esp_app_desc!();

const _: () = assert!(lattice_vertex::configSUPPORT_DYNAMIC_ALLOCATION == 0);
static mut REPORT_STACK: [StackType_t; 1024] = [0; 1024];
static mut REPORT_TCB: StaticTask_t = StaticTask_t::new();
static mut FIRST_STACK: [StackType_t; 512] = [0; 512];
static mut FIRST_TCB: StaticTask_t = StaticTask_t::new();
static mut SECOND_STACK: [StackType_t; 512] = [0; 512];
static mut SECOND_TCB: StaticTask_t = StaticTask_t::new();
static mut REPORT_HANDLE: TaskHandle_t = core::ptr::null_mut();

static ISR_SEEN: AtomicBool = AtomicBool::new(false);
static LOW_RESUMED: AtomicBool = AtomicBool::new(false);
static FIRST_RUNS: AtomicU32 = AtomicU32::new(0);
static SECOND_RUNS: AtomicU32 = AtomicU32::new(0);
static REGISTER_FAILURES: AtomicU32 = AtomicU32::new(0);

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority3)]
fn device_event() {
    unsafe {
        SoftwareInterrupt::<'static, 1>::steal().reset();
        ISR_SEEN.store(port::xPortIsInsideInterrupt() == pdTRUE, Ordering::Relaxed);
        let mut woken = pdFALSE;
        tasks::vTaskNotifyGiveFromISR(REPORT_HANDLE, &mut woken);
        port::portYIELD_FROM_ISR(woken);
    }
}

extern "C" fn report(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    assert_eq!(port::xPortIsInsideInterrupt(), pdFALSE);
    // The high-priority task must block before FIRST can raise its device IRQ.
    assert_eq!(unsafe { tasks::ulTaskNotifyTake(pdTRUE, portMAX_DELAY) }, 1);
    assert!(ISR_SEEN.load(Ordering::Relaxed));
    assert!(
        !LOW_RESUMED.load(Ordering::Relaxed),
        "ISR did not preempt the lower task"
    );

    let tick_start = tasks::xTaskGetTickCount();
    lattice_vertex::delay(&context, 25);
    assert!(tasks::xTaskGetTickCount().wrapping_sub(tick_start) >= 25);
    assert!(FIRST_RUNS.load(Ordering::Relaxed) > 0);
    assert!(SECOND_RUNS.load(Ordering::Relaxed) > 0);
    assert_eq!(REGISTER_FAILURES.load(Ordering::Relaxed), 0);
    println!("PASS static tasks, nested critical sections, ISR wakeup, tick preemption, register canaries");

    loop {
        lattice_vertex::delay(&context, 1000);
        assert_eq!(REGISTER_FAILURES.load(Ordering::Relaxed), 0);
        println!(
            "KERNEL tick={} task1={} task2={}",
            tasks::xTaskGetTickCount(),
            FIRST_RUNS.load(Ordering::Relaxed),
            SECOND_RUNS.load(Ordering::Relaxed)
        );
    }
}

extern "C" fn first(_: *mut c_void) {
    port::portENTER_CRITICAL();
    port::portENTER_CRITICAL();
    assert_eq!(port::xPortIsInsideInterrupt(), pdFALSE);
    unsafe { SoftwareInterrupt::<'static, 1>::steal() }.raise();
    port::portYIELD();
    port::portEXIT_CRITICAL();
    assert!(
        !ISR_SEEN.load(Ordering::Relaxed),
        "inner exit unmasked an outer critical section"
    );
    port::portEXIT_CRITICAL();
    LOW_RESUMED.store(true, Ordering::Relaxed);
    unsafe { register_loop(&FIRST_RUNS, &REGISTER_FAILURES, 0x1234) }
}

extern "C" fn second(_: *mut c_void) {
    unsafe { register_loop(&SECOND_RUNS, &REGISTER_FAILURES, 0x5678) }
}

// Two equal-priority tasks deliberately never yield. They can both advance only
// through tick preemption. Different caller/callee register canaries detect
// cross-task corruption that simple counters would miss.
extern "C" {
    fn register_loop(counter: &AtomicU32, failures: &AtomicU32, canary: u32) -> !;
}
core::arch::global_asm!(
    r#"
    .section .text.register_loop, "ax"
    .global register_loop
    .type register_loop, @function
register_loop:
    mv s2, a2
    mv t2, a2
1:
    bne s2, a2, 2f
    bne t2, a2, 2f
    lw t0, 0(a0)
    addi t0, t0, 1
    sw t0, 0(a0)
    j 1b
2:
    li t0, 1
    sw t0, 0(a1)
    j 2b
    .size register_loop, . - register_loop
"#
);

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let timer = SystemTimer::new(peripherals.SYSTIMER);
    let mut interrupts = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    port::esp32c6::initialize(timer.alarm0, interrupts.software_interrupt0);
    interrupts.software_interrupt1.reset();
    interrupts
        .software_interrupt1
        .set_interrupt_handler(device_event);

    // Fixture-owned tasks exercise the real kernel. Radio system-task setup
    // will live in the kernel when the common participation path is integrated.
    let context = unsafe { TaskContext::assume() };
    unsafe {
        let reporter = TaskHandle::spawn_static(
            &context,
            b"report",
            &mut REPORT_STACK,
            &mut REPORT_TCB,
            3,
            report,
        )
        .unwrap();
        REPORT_HANDLE = reporter.as_raw();
        TaskHandle::spawn_static(
            &context,
            b"first",
            &mut FIRST_STACK,
            &mut FIRST_TCB,
            2,
            first,
        )
        .unwrap();
        TaskHandle::spawn_static(
            &context,
            b"second",
            &mut SECOND_STACK,
            &mut SECOND_TCB,
            2,
            second,
        )
        .unwrap();
    }
    println!("START lattice kernel: {} (no heap)", port::portARCH_NAME);
    lattice_vertex::start_scheduler(context)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    port::portDISABLE_INTERRUPTS();
    println!("FAIL {info}");
    loop {
        core::hint::spin_loop();
    }
}
