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
/// `surface` borrows the appliance's static states, transformers, and IR predicates.
/// It is encoded without reading live state and published when this vertex arrives
/// or hears another arrival. No foreign-surface registry is created.
///
/// # Panics
/// Before scheduler startup, rejects an encoded surface over 65,535 bytes or
/// expression nesting reaching the encoder's depth limit (32).
#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub fn start<A: 'static, E: 'static>(
    context: crate::sync::TaskContext,
    radio: crate::port::RadioPeripheral,
    surface: &'static crate::surface::Surface<'static, A, E>,
) -> ! {
    let publication =
        peer::exposure::Publication::new(surface).expect("surface exceeds publication limits");
    radio::start(&context, radio, publication);
    crate::start_scheduler(context)
}
