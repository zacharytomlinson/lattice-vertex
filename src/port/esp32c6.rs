/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 *
 * ESP32-C6 adaptation of this repository's RV32 FreeRTOS port. The direct
 * software-interrupt integration follows esp-hal/esp-rtos's supported API;
 * scheduling and task storage remain owned by this kernel.
 */

//! ESP32-C6 high-power core port, using esp-hal for startup and interrupts.
//!
//! After `esp_hal::init`, pass SYSTIMER alarm 0 and software interrupt 0 to
//! [`initialize`], create static tasks, then call [`crate::start_scheduler`].
//! The port owns these resources until reset. Other peripherals and interrupts
//! remain available through the HAL. No ESP-IDF or esp-rtos scheduler is used.
//!
//! Peripheral handlers use the interrupted task's stack. Context switching uses
//! its own static stack and the lowest interrupt priority, so it cannot switch
//! away from a nested peripheral handler. All kernel FromISR operations mask
//! interrupts while touching scheduler state. Interrupts must remain bounded;
//! masking them across several tick periods loses elapsed ticks in this first
//! periodic-timer port. Tick suppression is not implemented yet.

use core::{arch::asm, ffi::c_void};

use esp_hal::{
    interrupt::{
        self, software::SoftwareInterrupt, DirectBindableCpuInterrupt, Priority, RunLevel,
    },
    peripherals::Interrupt,
    time::Duration,
    timer::{systimer::Alarm, PeriodicTimer},
    Blocking,
};

use crate::{config::configTICK_RATE_HZ, kernel::tasks, types::*};

pub const portSTACK_GROWTH: BaseType_t = -1;
pub const portBYTE_ALIGNMENT: usize = 16;
pub const portARCH_NAME: &str = "ESP32-C6 RV32IMAC";

// x1..x31 (x2's slot is unused), mepc, mstatus, and three padding words.
// Keep this in sync with context.S. Both saved and restored SP are ABI aligned.
const CONTEXT_WORDS: usize = 36;
const INITIAL_MSTATUS: u32 = 0x1880; // Machine mode, interrupts enabled by mret.

#[repr(C, align(16))]
struct SwitchStack([u8; 2048]);
static mut SWITCH_STACK: SwitchStack = SwitchStack([0; 2048]);
static mut TICK_TIMER: Option<PeriodicTimer<'static, Blocking>> = None;
static mut CRITICAL_NESTING: usize = 0;
static mut CRITICAL_MSTATUS: UBaseType_t = 0;
static mut SWITCHING: bool = false;

core::arch::global_asm!(include_str!("esp32c6/context.S"));

extern "C" {
    fn lattice_c6_switch();
    fn lattice_c6_restore() -> !;
}

/// Transfer the scheduler's hardware resources to the port before creating tasks.
///
/// HAL initialization must have completed. The timer interrupt is armed only
/// when the scheduler starts, after its ready lists and current TCB are valid.
/// Resource ownership prevents another safe HAL user from reconfiguring them.
pub fn initialize(alarm: Alarm<'static>, software_interrupt: SoftwareInterrupt<'static, 0>) {
    assert_eq!(
        tasks::xTaskGetSchedulerState(),
        tasks::taskSCHEDULER_NOT_STARTED
    );
    assert!(RunLevel::current().is_thread());
    let mask = portSET_INTERRUPT_MASK_FROM_ISR();
    unsafe {
        assert!(TICK_TIMER.is_none(), "C6 port already initialized");
        let mut timer = PeriodicTimer::new(alarm);
        timer.unlisten();
        timer.set_interrupt_handler(tick);
        TICK_TIMER = Some(timer);
    }
    software_interrupt.reset();
    interrupt::enable_direct(
        Interrupt::FROM_CPU_INTR0,
        Priority::min(),
        DirectBindableCpuInterrupt::Interrupt0,
        lattice_c6_switch,
    );
    portCLEAR_INTERRUPT_MASK_FROM_ISR(mask);
}

#[inline(always)]
pub fn portDISABLE_INTERRUPTS() {
    unsafe { asm!("csrci mstatus, 8", options(nostack)) };
}

#[inline(always)]
pub fn portENABLE_INTERRUPTS() {
    unsafe { asm!("csrsi mstatus, 8", options(nostack)) };
}

#[inline(always)]
pub fn portSET_INTERRUPT_MASK_FROM_ISR() -> UBaseType_t {
    let saved;
    unsafe { asm!("csrrci {}, mstatus, 8", out(reg) saved, options(nostack)) };
    saved
}

#[inline(always)]
pub fn portCLEAR_INTERRUPT_MASK_FROM_ISR(saved: UBaseType_t) {
    if saved & 8 != 0 {
        portENABLE_INTERRUPTS();
    }
}

pub fn portENTER_CRITICAL() {
    let saved = portSET_INTERRUPT_MASK_FROM_ISR();
    unsafe {
        if CRITICAL_NESTING == 0 {
            CRITICAL_MSTATUS = saved;
        }
        CRITICAL_NESTING += 1;
    }
}

pub fn portEXIT_CRITICAL() {
    unsafe {
        assert!(CRITICAL_NESTING > 0, "unbalanced critical section");
        CRITICAL_NESTING -= 1;
        if CRITICAL_NESTING == 0 {
            portCLEAR_INTERRUPT_MASK_FROM_ISR(CRITICAL_MSTATUS);
        }
    }
}

#[inline]
pub fn portYIELD() {
    // initialize transferred this source to the port. Raising it while masked
    // defers switching until the outer critical section / HAL ISR has exited.
    unsafe { SoftwareInterrupt::<'static, 0>::steal() }.raise();
}

#[inline]
pub fn portYIELD_FROM_ISR(required: BaseType_t) {
    if required != pdFALSE {
        portYIELD();
    }
}

pub use portYIELD_FROM_ISR as portEND_SWITCHING_ISR;

pub fn xPortIsInsideInterrupt() -> BaseType_t {
    // RunLevel tracks the HAL's nested IRQ dispatch, not mstatus.MIE; a masked
    // task critical section must still be recognized as task context.
    (!RunLevel::current().is_thread() || unsafe { SWITCHING }) as BaseType_t
}

/// Construct the register frame consumed by context.S.
///
/// # Safety
/// `top` must belong to unique, stationary task stack storage with room below it
/// for the aligned context frame. `parameter` must satisfy `entry` for the task's
/// entire lifetime. The entry function must not return.
pub unsafe fn pxPortInitialiseStack(
    top: *mut StackType_t,
    entry: TaskFunction_t,
    parameter: *mut c_void,
) -> *mut StackType_t {
    let frame = ((top as usize & !15) as *mut StackType_t).sub(CONTEXT_WORDS);
    frame.write_bytes(0, CONTEXT_WORDS);
    frame.add(0).write(task_returned as *const () as u32);
    let gp: u32;
    let tp: u32;
    asm!("mv {}, gp", out(reg) gp, options(nomem, nostack));
    asm!("mv {}, tp", out(reg) tp, options(nomem, nostack));
    frame.add(2).write(gp);
    frame.add(3).write(tp);
    frame.add(9).write(parameter as u32); // a0 = x10
    frame.add(31).write(entry as *const () as u32);
    frame.add(32).write(INITIAL_MSTATUS);
    frame
}

extern "C" fn task_returned() -> ! {
    panic!("an RTOS task returned");
}

pub fn xPortStartScheduler() -> BaseType_t {
    portDISABLE_INTERRUPTS();
    unsafe {
        assert!(CRITICAL_NESTING == 0);
        let timer = TICK_TIMER
            .as_mut()
            .expect("initialize the C6 port before starting tasks");
        assert!(1_000_000 % configTICK_RATE_HZ == 0);
        timer
            .start(Duration::from_micros(
                (1_000_000 / configTICK_RATE_HZ) as u64,
            ))
            .unwrap();
        timer.listen();
        // No interrupts can see partially initialized kernel state. The first
        // task's mret enables them after restoring its stack and registers.
        lattice_c6_restore()
    }
}

pub fn vPortEndScheduler() {
    panic!("ESP32-C6 scheduler cannot return to the boot stack");
}

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority2)]
fn tick() {
    let saved = portSET_INTERRUPT_MASK_FROM_ISR();
    unsafe {
        TICK_TIMER.as_mut().unwrap().clear_interrupt();
        portYIELD_FROM_ISR(tasks::xTaskIncrementTick());
    }
    portCLEAR_INTERRUPT_MASK_FROM_ISR(saved);
}

// Called with MIE clear by the direct software IRQ on the private switch stack.
#[no_mangle]
extern "C" fn lattice_c6_select_task() {
    unsafe {
        SWITCHING = true;
        SoftwareInterrupt::<'static, 0>::steal().reset();
        tasks::vTaskSwitchContext();
        SWITCHING = false;
    }
}

// A symbol operand avoids exposing the switch stack as a public mutable static.
core::arch::global_asm!(
    ".set lattice_c6_switch_stack_top, {stack} + 2048",
    stack = sym SWITCH_STACK,
);

#[inline(always)]
pub fn portNOP() {
    unsafe { asm!("nop", options(nomem, nostack)) };
}

#[inline(always)]
pub fn portMEMORY_BARRIER() {
    unsafe { asm!("fence iorw, iorw", options(nostack)) };
}

#[cfg(feature = "generate-run-time-stats")]
pub fn portCONFIGURE_TIMER_FOR_RUN_TIME_STATS() {}

#[cfg(feature = "generate-run-time-stats")]
pub fn portGET_RUN_TIME_COUNTER_VALUE() -> u32 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros() as u32
}

#[cfg(feature = "tickless-idle")]
pub fn vPortSuppressTicksAndSleep(_expected_idle_ticks: TickType_t) {
    // Keep the periodic tick running until a C6 tick-suppression path exists.
    unsafe { asm!("wfi", options(nostack)) };
}
