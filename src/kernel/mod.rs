/*
 * FreeRTOS Kernel <DEVELOPMENT BRANCH>
 * Copyright (C) 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * SPDX-License-Identifier: MIT
 *
 * [AMENDMENT] This module contains the core FreeRTOS kernel components,
 * translated from the original C source files:
 * - list.rs   (from list.c)
 * - queue.rs  (from queue.c)
 * - tasks.rs  (from tasks.c)
 * - timers.rs (from timers.c)
 * - event_groups.rs (from event_groups.c)
 * - stream_buffer.rs (from stream_buffer.c)
 */

//! Lattice kernel runtime and FreeRTOS-derived execution machinery.
//!
//! `kernel::start` establishes the resident radio task before entering the task
//! scheduler on supported MCU ports. [`radio`] owns transport and wakeups.
//! The execution machinery includes:
//!
//! - [`list`] - Linked list implementation used by the scheduler
//! - `queue` - Queues, semaphores, and mutexes
//! - `tasks` - Task management and scheduler
//! - `timers` - Optional software timer service
//! - `event_groups` - Event group synchronization
//! - `stream_buffer` - Optional stream and message buffers

pub mod event_groups;
pub mod list;
pub(crate) mod peer;
pub mod queue;
pub mod radio;
#[cfg(feature = "stream-buffers")]
pub mod stream_buffer;
pub mod tasks;
#[cfg(feature = "timers")]
pub mod timers;

/// Establish the kernel-owned radio task and start local execution.
/// Hardware clocks/CPU-port initialization and any appliance tasks must be ready.
/// The resident radio enters common peer discovery. Appliance code supplies no
/// channel, PAN, discovery loop, or diagnostic workload.
#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub fn start(context: crate::sync::TaskContext, radio: crate::port::RadioPeripheral) -> ! {
    radio::start(&context, radio);
    crate::start_scheduler(context)
}
