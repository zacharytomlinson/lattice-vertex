# Lattice vertex

An event-driven appliance kernel built on FreeRTOS-derived task, interrupt and
synchronization primitives. Firmware implements the appliance's state machine;
the kernel handles network participation, state deltas, installed automations and
native operation dispatch. Mathematical IR models are optional.

## Start an appliance

Use this checkout as a dependency while developing a sibling firmware project:

```toml
[dependencies]
lattice-vertex = { path = "../lattice-vertex", default-features = false, features = ["port-esp32c6"] }
```

Select exactly one port. Both supported radio ports include `use-mutexes` and work
without a heap allocator, USB, test hooks or an IR model.

| Chip | Feature | Rust target |
| --- | --- | --- |
| ESP32-C6 | `port-esp32c6` | `riscv32imac-unknown-none-elf` |
| nRF52840 | `port-nrf52840` | `thumbv7em-none-eabihf` |

The default `port-dummy` is compile-only; disable default features in firmware.
Use a published, pinned Git revision for reproducible remote builds. A Git
dependency does not include uncommitted edits in a sibling checkout; use the path
dependency during joint firmware/kernel development. The shared `lattice-ir`
dependency is pinned in the manifest and requires Git access. Consumption is by
path or Git; crates.io publication is disabled in the manifest.

Read [Writing appliance firmware](src/kernel/appliance/README.md) for the interface.
The maintained [lamp example](examples/surface.rs) shows a complete native-only
declaration with ordinary methods, automatic state registration and no IR boilerplate:

```sh
cargo run --locked --no-default-features --features port-test,use-mutexes --example surface
```

For a relay, declare its power state, expose its native power-setting method, and
connect the physical switch/feedback handler to its state primitive. Use
`State<bool>` for an authoritative software state or `StateEvent<bool>` paired with
a hardware reader. Firmware handles the electrical output, feedback and debounce.
No network loop, delta-publish call or automation knowledge belongs in that code.

The firmware binary owns its reset entry, board/HAL initialization, linker layout
and panic handler. Initialize the appliance, enable its device callbacks, create
any needed appliance tasks, then enter `kernel::start`. The
[startup section](src/kernel/appliance/README.md#startup) identifies the port resources
and existing board-entry references. Do not copy a board test's reporter, USB console,
test workload or update signing policy into an appliance.

## Development checks

```sh
cargo test --locked --no-default-features --features port-test,use-mutexes -- --test-threads=1
cargo doc --locked --no-default-features --features port-test,use-mutexes --no-deps
```

The host port has global scheduler state, so its checks run serially.
[Port checks](src/port/TESTING.md) own MCU build/link and hardware procedures.
Build success and host properties do not establish physical relay behavior.

[Documentation ownership](docs/README.md) links the architecture, current direction
and component contracts. Next comes real appliance integration, beginning with the
relay; LLVM lowering and a hardware circuit backend remain later work.

The inherited FreeRTOS attribution and MIT license remain in the source.
