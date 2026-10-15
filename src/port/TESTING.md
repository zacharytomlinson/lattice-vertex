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
| [Surface reads](../kernel/peer/EXPOSURE.md) | Compact owner discovery, bounded requested ranges, and relay forwarding |
| [Shared radio fixture](../kernel/radio/tests/README.md) | Controlled probe workload and passive discovery counters used by both MCU test images |

The radio probe replaces only the resident task's message workload through the
kernel's gated test entry. It still exercises the real driver and task machinery.
The discovery observer leaves the ordinary kernel handler in control. Neither
requires an experimental firmware project to build or run.

Standalone vendor/Wi-Fi controls, raw RF register comparisons, antenna trials,
room-by-room captures, and appliance-specific adapters belong in an external lab.
They are not shipped as kernel source or enabled by extra diagnostic features.
Supported-board linker layouts, upload packaging, and logging stay with the port
fixtures because they make these kernel checks reproducible on that hardware.
