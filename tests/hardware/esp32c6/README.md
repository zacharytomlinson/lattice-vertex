# ESP32-C6 kernel smoke test

This image starts Lattice's FreeRTOS-derived scheduler through `port-esp32c6`.
It uses three static tasks plus the kernel's static idle task, with no allocator.
It is a hardware test, not an appliance example or a radio protocol implementation.

From this directory:

```sh
cargo +stable build --release --locked --offline
espflash flash --monitor --port /dev/ttyACM0 target/riscv32imac-unknown-none-elf/release/lattice-c6-kernel-smoke
```

Select the connected board's actual serial port. Flashing replaces its application.
No automatic flashing is part of the build.

The expected output is `START`, then `PASS`, followed by periodic `KERNEL` lines.
The assertions exercise nested critical sections, an ordinary HAL interrupt
waking a blocked higher-priority task before the lower task resumes, tick-based
delay, and preemption between two equal-priority tasks that never yield. Different
caller-saved and callee-saved register canaries detect cross-task corruption.
`FAIL` reports an assertion/panic. `START` without `PASS` is not a passing test.

The port reserves SYSTIMER alarm 0 and software interrupt 0. The fixture uses
software interrupt 1 as a device-event source. Actual radio reception and its
buffering are separate integration work; this test does not claim packet delivery.
