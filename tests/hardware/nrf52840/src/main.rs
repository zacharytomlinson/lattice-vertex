//! Allocator-free Nordic scheduler test. Logging uses RTT through an SWD probe.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use cortex_m::peripheral::NVIC;
use cortex_m_rt::entry;
use hal::interrupt;
use lattice_vertex::{
    kernel::tasks::{self, StaticTask_t},
    port::{
        self,
        nrf52840::{self, hal},
    },
    sync::{TaskContext, TaskHandle},
    types::*,
};
use rtt_target::{rprintln, rtt_init_print};

const _: () = assert!(lattice_vertex::configSUPPORT_DYNAMIC_ALLOCATION == 0);
static mut REPORT_STACK: [StackType_t; 512] = [0; 512];
static mut REPORT_TCB: StaticTask_t = StaticTask_t::new();
static mut FIRST_STACK: [StackType_t; 256] = [0; 256];
static mut FIRST_TCB: StaticTask_t = StaticTask_t::new();
static mut SECOND_STACK: [StackType_t; 256] = [0; 256];
static mut SECOND_TCB: StaticTask_t = StaticTask_t::new();
static mut REPORT_HANDLE: TaskHandle_t = core::ptr::null_mut();
static ISR_SEEN: AtomicBool = AtomicBool::new(false);
static LOW_RESUMED: AtomicBool = AtomicBool::new(false);
static FIRST_RUNS: AtomicU32 = AtomicU32::new(0);
static SECOND_RUNS: AtomicU32 = AtomicU32::new(0);
static REGISTER_FAILURES: AtomicU32 = AtomicU32::new(0);

#[interrupt]
fn EGU0_SWI0() {
    // This fixture pends the NVIC vector directly; no EGU event needs clearing.
    ISR_SEEN.store(port::xPortIsInsideInterrupt() == pdTRUE, Ordering::Relaxed);
    let mut woken = pdFALSE;
    unsafe { tasks::vTaskNotifyGiveFromISR(REPORT_HANDLE, &mut woken) };
    port::portYIELD_FROM_ISR(woken);
}

extern "C" fn report(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    assert_eq!(port::xPortIsInsideInterrupt(), pdFALSE);
    assert_eq!(cortex_m::peripheral::SYST::get_reload(), 63_999);
    assert_eq!(unsafe { tasks::ulTaskNotifyTake(pdTRUE, portMAX_DELAY) }, 1);
    assert!(ISR_SEEN.load(Ordering::Relaxed));
    assert!(
        !LOW_RESUMED.load(Ordering::Relaxed),
        "ISR did not preempt the lower task"
    );
    let start = tasks::xTaskGetTickCount();
    lattice_vertex::delay(&context, 25);
    assert!(tasks::xTaskGetTickCount().wrapping_sub(start) >= 25);
    assert!(FIRST_RUNS.load(Ordering::Relaxed) > 0);
    assert!(SECOND_RUNS.load(Ordering::Relaxed) > 0);
    assert_eq!(REGISTER_FAILURES.load(Ordering::Relaxed), 0);
    rprintln!("PASS static tasks, 64MHz tick, nested critical sections, ISR wakeup, preemption, integer/FPU canaries");
    loop {
        lattice_vertex::delay(&context, 1000);
        assert_eq!(REGISTER_FAILURES.load(Ordering::Relaxed), 0);
        rprintln!(
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
    NVIC::pend(interrupt::EGU0_SWI0);
    port::portYIELD();
    port::portEXIT_CRITICAL();
    assert!(
        !ISR_SEEN.load(Ordering::Relaxed),
        "inner exit unmasked the outer critical section"
    );
    port::portEXIT_CRITICAL();
    LOW_RESUMED.store(true, Ordering::Relaxed);
    unsafe { register_loop(&FIRST_RUNS, &REGISTER_FAILURES, 0x1234) }
}

extern "C" fn second(_: *mut c_void) {
    unsafe { register_loop(&SECOND_RUNS, &REGISTER_FAILURES, 0x5678) }
}

// These tasks never yield. Both counters can advance only by tick preemption.
// r4 and s16 use the software context save; r2 and s0 use exception stacking.
// Each task has different integer and FPU bit patterns.
extern "C" {
    fn register_loop(counter: &AtomicU32, failures: &AtomicU32, canary: u32) -> !;
}
core::arch::global_asm!(
    r#"
    .syntax unified
    .thumb
    .fpu fpv4-sp-d16
    .section .text.register_loop, "ax"
    .global register_loop
    .type register_loop, %function
    .thumb_func
register_loop:
    mov r4, r2
    vmov s0, r2
    vmov s16, r2
1:
    cmp r4, r2
    bne 2f
    vmov r3, s0
    cmp r3, r2
    bne 2f
    vmov r3, s16
    cmp r3, r2
    bne 2f
    ldr r3, [r0]
    adds r3, #1
    str r3, [r0]
    b 1b
2:
    movs r3, #1
    str r3, [r1]
    b 2b
    .size register_loop, . - register_loop
"#
);

#[entry]
fn main() -> ! {
    rtt_init_print!();
    let mut core = cortex_m::Peripherals::take().unwrap();
    let _peripherals = nrf52840::initialize(core.SYST);
    // Only the fixture's device event is bound here; the port owns all scheduler
    // vectors. The device IRQ can enter the kernel at Nordic priority P5.
    NVIC::unpend(interrupt::EGU0_SWI0);
    unsafe {
        core.NVIC.set_priority(
            interrupt::EGU0_SWI0,
            nrf52840::KERNEL_INTERRUPT_PRIORITY as u8,
        );
        NVIC::unmask(interrupt::EGU0_SWI0);
    }
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
    rprintln!("START lattice kernel: {} (no heap)", port::portARCH_NAME);
    lattice_vertex::start_scheduler(context)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    cortex_m::interrupt::disable();
    rprintln!("FAIL {}", info);
    loop {
        cortex_m::asm::wfi();
    }
}
