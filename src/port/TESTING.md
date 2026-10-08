# Lattice kernel hardware checks

Hardware checks live in each port's `tests/` directory. The C6 and Nordic checks
are separate firmware crates with their own MCU entry points, target settings,
and linker configuration. Their location expresses port ownership; they are not
modules or dependencies of an ordinary kernel build. Shared transport workload
code lives beside `kernel::radio`, and discovery notes beside `kernel::peer`.

| Check | What it exercises |
| --- | --- |
| [C6](esp32c6/tests/README.md) and [Nordic](nrf52840/tests/README.md) smoke binaries | Static tasks, critical sections, interrupt wakeups, preemption, and register preservation |
| [Radio probe](../kernel/radio/TESTING.md) | Selected radio port, resident kernel task, IRQ notifications, receive/transmit handling, and bounded buffers |
| [Peer discovery](../kernel/peer/TESTING.md) | Ordinary kernel channel selection, rendezvous, discovery, and founder-independent participation |
| [Surface publication](../kernel/peer/EXPOSURE.md) | Symmetric publication on arrival, complete IR descriptors, and no periodic publication loop |
| [Shared radio fixture](../kernel/radio/tests/README.md) | Controlled probe workload and passive discovery counters used by both MCU test images |

The radio probe replaces only the resident task's message workload through the
kernel's gated test entry. It still exercises the real driver and task machinery.
The discovery observer leaves the ordinary kernel handler in control. Neither
requires an experimental firmware project to build or run.

Standalone vendor/Wi-Fi controls, raw RF register comparisons, and antenna-wiring
trials belong in [temporary experiments](../../experiments/esp32c6-rf/README.md).
They investigate hardware/PHY hypotheses and do not establish kernel behavior.
