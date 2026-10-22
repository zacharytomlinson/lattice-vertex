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

#[cfg(feature = "use-mutexes")]
mod appliance;
#[cfg(feature = "use-mutexes")]
pub use appliance::{Appliance, InstallError};
#[cfg(feature = "use-mutexes")]
pub(crate) mod change;
pub mod event_groups;
pub mod list;
pub(crate) mod peer;
#[cfg(feature = "firmware-update")]
pub mod update;
#[cfg(feature = "host-link")]
pub use peer::host;
pub mod queue;
pub mod radio;
#[cfg(feature = "use-mutexes")]
pub mod state;
#[cfg(feature = "stream-buffers")]
pub mod stream_buffer;
pub mod tasks;
#[cfg(feature = "timers")]
pub mod timers;

/// Establish the kernel-owned radio task and start local execution.
/// Hardware clocks/CPU-port initialization and the passive appliance binding must
/// be ready. The kernel creates acquisition, native-call and radio tasks.
/// The resident radio enters common peer discovery. Appliance code supplies no
/// channel, PAN, discovery loop, or diagnostic workload.
/// `appliance` owns the initialized native instance and borrows its static surface.
/// Complete local retained-state installation first; startup seals
/// those bindings and exposes the combined native and installed state definitions.
/// The resident task serves definitions, invokes exposed native transformers,
/// and sends registered observation deltas. Relays retain bounded directions and
/// duplicate history, never foreign surfaces or an appliance-state mirror.
///
/// # Panics
/// Before scheduler startup, rejects uninitialized/already-bound appliances, an
/// encoded surface over 65,535 bytes, or expression nesting reaching 32 levels.
#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub fn start<A: Send + Sync + 'static, E: 'static>(
    context: crate::sync::TaskContext,
    radio: crate::port::RadioPeripheral,
    appliance: &'static Appliance<A, E>,
) -> ! {
    let description = peer::exposure::Description::with_installed(appliance)
        .expect("surface exceeds description limits");
    change::bind(appliance);
    appliance::execution::start(&context, appliance);
    radio::start(&context, radio, description, appliance);
    crate::start_scheduler(context)
}

/// Start normal peer participation with the port's reserved update storage and
/// a build-provisioned release policy. No update packet handling belongs to the appliance.
#[cfg(all(
    feature = "firmware-update",
    any(
        all(feature = "port-nrf52840", feature = "nrf52840-update"),
        all(feature = "port-esp32c6", feature = "esp32c6-update")
    )
))]
pub fn start_with_updates<A: Send + Sync + 'static, E: 'static>(
    context: crate::sync::TaskContext,
    radio: crate::port::RadioPeripheral,
    appliance: &'static Appliance<A, E>,
    storage: update::PlatformStorage,
    policy: update::package::Policy,
) -> ! {
    assert_eq!(
        policy.surface,
        update::appliance_surface_identity(appliance),
        "release policy does not match the exposed surface"
    );
    update::runtime::initialize(storage, policy);
    start(context, radio, appliance)
}
