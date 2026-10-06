// SPDX-License-Identifier: MIT

//! Nordic nRF52840 port: chip initialization and kernel exception bindings.
//!
//! The existing Cortex-M4F port owns task/FPU contexts, BASEPRI critical
//! sections, SysTick, and PendSV. This module supplies the nRF clock setup and
//! vectors, so applications neither install kernel handlers nor provide clock
//! constants. `embassy-nrf` supplies peripheral initialization and errata fixes;
//! no Embassy executor, time driver, Nordic SoftDevice, or ESP code is needed.
//!
//! Call [`initialize`] once from the reset entry before creating static tasks.
//! Boards must provide the radio's 32 MHz crystal. No external 32 kHz crystal is
//! required. SysTick is reserved for the kernel; Nordic TIMER/RTC peripherals
//! remain available. IRQs calling kernel FromISR APIs must use priorities 5..=7.

use core::sync::atomic::{AtomicBool, Ordering};
use cortex_m::peripheral::SYST;

pub use super::cortex_m4f::*;
pub use embassy_nrf as hal;

use crate::{kernel::tasks, types::BaseType_t};

pub const portARCH_NAME: &str = "nRF52840 Cortex-M4F";

/// Highest urgency allowed for Nordic interrupts that enter the kernel.
pub const KERNEL_INTERRUPT_PRIORITY: hal::interrupt::Priority = hal::interrupt::Priority::P5;

const _: () = assert!(hal::NVIC_PRIO_BITS == 3);
const _: () = assert!(crate::config::configCPU_CLOCK_HZ == 64_000_000);
const _: () = assert!(
    crate::config::configMAX_SYSCALL_INTERRUPT_PRIORITY == KERNEL_INTERRUPT_PRIORITY as u32
);

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the nRF52840 and transfer SysTick to the kernel.
///
/// Returns the Nordic peripherals for appliance I/O. The kernel starts SysTick
/// only when its scheduler starts. This calls `embassy_nrf::init` internally;
/// appliance code must not also initialize that HAL separately.
pub fn initialize(mut systick: SYST) -> hal::Peripherals {
    assert_eq!(
        tasks::xTaskGetSchedulerState(),
        tasks::taskSCHEDULER_NOT_STARTED
    );
    assert_eq!(xPortIsInsideInterrupt(), crate::types::pdFALSE);
    systick.disable_interrupt();
    systick.disable_counter();
    systick.clear_current();

    let mut config = hal::config::Config::default();
    config.hfclk_source = hal::config::HfclkSource::ExternalXtal;
    // Retain internal LFCLK and disabled DC/DC defaults: neither a second
    // crystal nor board-specific regulator inductors are needed for this port.
    let peripherals = hal::init(config);
    INITIALIZED.store(true, Ordering::Release);
    peripherals
}

/// Enter the shared Cortex-M4F scheduler after Nordic initialization.
pub fn xPortStartScheduler() -> BaseType_t {
    assert!(
        INITIALIZED.load(Ordering::Acquire),
        "initialize the nRF52840 port before starting tasks"
    );
    super::cortex_m4f::xPortStartScheduler()
}

// Retain the shared port's exception handlers and its cortex-m-rt assembly
// aliases. This belongs to the platform port, not each appliance's entry point.
#[used]
static EXCEPTION_HANDLERS: [unsafe extern "C" fn(); 3] =
    [vPortSVCHandler, xPortPendSVHandler, xPortSysTickHandler];
