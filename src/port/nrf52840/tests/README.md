# nRF52840 kernel smoke test

For the integrated radio path, use the [radio test](../../../kernel/radio/TESTING.md).
For ordinary kernel peer discovery with passive logging, use the
[discovery check](../../../kernel/peer/TESTING.md).

The radio port adds receive-only energy detection beside the HAL packet driver.
It uses `READY_EDSTART` / `EDEND_DISABLE`, `EDCNT+1` windows of 128 us, and the
[nRF52840 ED conversion](https://docs.nordicsemi.com/r/bundle/ps_nrf52840/page/radio.html)
`-92 + 4 * EDSAMPLE` dBm. Disabled-event wakeups share the resident task's interrupt
binding with the HAL. Cancellation disables the radio before another operation.
This path is build-checked and awaits hardware execution.

This image runs Lattice's existing Cortex-M4F scheduler through `port-nrf52840`.
The port supplies Nordic clock setup, kernel exception bindings, and interrupt
priority limits. The image has no allocator, ESP dependencies, SoftDevice,
Embassy executor, or Embassy time driver.

From this directory, with `thumbv7em-none-eabihf` installed:

```sh
cargo +stable build --release --locked
probe-rs run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/release/lattice-nrf52840-kernel-smoke
```

Logging uses RTT through an SWD probe, so no board-specific LED or UART pin map
is needed. `memory.x` describes a standalone image beginning at flash address
zero, with 1 MiB flash and 256 KiB RAM. It does not reserve a preinstalled
bootloader or SoftDevice. USB DFU/UF2 boards need their corresponding memory
layout and image packaging; this is an SWD test image.

Expected output is `START`, then `PASS`, followed by periodic `KERNEL` lines.
`START` alone is not a passing test. Assertions check:

- Automatic scheduler vector binding and the 64 MHz / 1 kHz SysTick reload.
- Nested critical sections and a device ISR waking a blocked higher-priority task.
- Tick preemption between equal-priority tasks that never yield.
- Distinct integer and floating-point register canaries across task switches.

The fixture uses the EGU0/SWI0 interrupt as a software-pended device event at
the port's kernel-callable priority. It does not test radio reception. The HAL
starts the required 32 MHz crystal; no external 32 kHz crystal is assumed.

For environments with Rust sources but without the prebuilt target component:

```sh
cargo +nightly build -Z build-std=core --release --locked
```
