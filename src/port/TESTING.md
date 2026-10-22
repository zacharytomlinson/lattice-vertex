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

## Update checks

Build and provisioning are owned by the [Nordic](nrf52840/tests/UPDATE.md) and
[C6](esp32c6/tests/UPDATE.md) procedures. The common updater owns
[package and runtime behavior](../../docs/FIRMWARE_UPDATES.md); the
[tool guide](../../tools/README.md) owns signing and timing measurements.
The following commands run from the repository root on an explicitly selected
board. Preparation builds images; the runners below stage and boot them.

| Fixture | Boundary exercised |
| --- | --- |
| Local USB `update` A/B | Flash bounds, digest check, trial, explicit confirmation and reversion |
| Signed `ota` A/B | Package admission, checkpoint/resume and ordinary radio delivery |
| `--activation-check` | Physical-work hold, fresh quiet interval and autonomous selection |
| `--watchdog-check` | Stalled-trial reset/reversion and healthy automatic confirmation |
| `--staging-check` | Flash admission hold, Busy without progress, responsive status and incremental verification |

Both ports have physical lifecycle evidence, signed transfers in both one-hop
directions, watchdog recovery, and local paced-staging checks. This is not a
multi-hop, real-actuator, loss-free-radio, or release-timing guarantee. Physical
power removal *during* an individual flash write or swap remains separate from
reset/resume and host fault injection. Detailed captures and measured run tables
are preserved in the external lab, not maintained as a second runtime specification.
Standalone CPU context smoke binaries have build validation; that does not imply
all listed hardware checks have run on every revision.

### Activation check

Use the port's `prepare --ota --activation-check`. A holds activation for ten
seconds after verification through the public hold API; B is an ordinary signed
receiver. Sign B with measured costs, provision A, and upload over the radio path.
Require A through the hold and a fresh quiet/proposal interval, followed by B
without a host Activate command. B confirms healthy local startup automatically;
reset and require B to remain. This checks the local policy, not future network
idleness or the timing of an actual physical appliance.

### Watchdog check

Use `prepare --ota --watchdog-check` and provision its loader plus A. Sign
`stall-entry.bin`, `stall-radio.bin`, and `b.bin` as release 2 with explicit
`--unmeasured`, naming packages `stall-entry-watchdog.lvup`,
`stall-radio-watchdog.lvup`, and `b-watchdog.lvup` in the port's `target/ota`.

```sh
python src/port/tests/update_watchdog.py --port ACTUAL_PORT \
  --images PORT/tests/target/ota --minimum-recovery-seconds 25
```

Replace `PORT` with `src/port/esp32c6` or `src/port/nrf52840`. Nordic needs a minimum
of 65 seconds instead of 25 to include its full-slot swaps. The stalled candidates
mask interrupts and spin before peripheral initialization or in the existing
DiscoveryStarted test hook. They perform no software reset or panic recovery.
The runner selects/resets each trial, waits for hardware expiry and restoration
without further reset/Confirm commands, then requires healthy B to remain after
a second reset without host confirmation. The C6 HAL handoff gap remains documented
in [startup confirmation](../../docs/FIRMWARE_UPDATES.md#watchdog-backed-startup-confirmation).

### Staging check

Use `prepare --ota --staging-check`, provision A and sign B with measured costs:

```sh
python src/port/tests/update_staging.py --port ACTUAL_PORT --package PACKAGE
```

A holds background updates for ten seconds after accepting a package. The runner
requires Busy with frozen progress and responsive status, then bounded verification,
selection and retained B without host confirmation. This is a local receiver/flash
check; it does not measure RF loss or physical appliance latency during an erase.

### Local command framing

Both low-level USB fixtures share the maintained command decoder. Its portable
bounds/checksum checks can run without a board:

```sh
rustc --edition 2021 --test src/port/tests/update_commands.rs -o /tmp/lattice-update-command-tests
/tmp/lattice-update-command-tests
```

This diagnostic console is separate from the signed receiver's binary host link.
Neither fixture's command grammar is a permanent kernel API.
