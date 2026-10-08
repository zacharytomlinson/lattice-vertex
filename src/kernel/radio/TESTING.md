# Kernel radio receive/reply test

The `radio` binaries in the C6 and Nordic ports' `tests/` crates use the shared
[`lattice-radio-test`](tests) workload. That crate owns probe encoding, cadence,
replies, channel/PAN test settings, and counter storage. The board binaries supply
CPU initialization and logging; no board-A/board-B build is needed.

The workload enters the kernel through `test-support`, runs on the real resident
radio task, and uses its driver operations and task-notification waits. Gated
event hooks report wait entries, ISR wakeups, RX evictions, and available receive
signal/abort metadata to fixture counters.
The kernel does not depend on this crate. Normal builds contain no probe protocol
or counter storage, and do not expose the test-only transport entry.

From `src/port/esp32c6/tests`:

```sh
cargo +stable build --release --locked --features radio-probe --bin radio
espflash flash --monitor --port /dev/ttyACM0 target/riscv32imac-unknown-none-elf/release/radio
```

From `src/port/nrf52840/tests`, with an SWD probe:

```sh
cargo +stable build --release --locked --features radio-probe --bin radio
probe-rs run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/release/radio
```

Nordic's standalone memory layout begins at flash address zero. This image is
for SWD loading and RTT output; a board's USB/UF2 bootloader needs its own image
layout/packaging. Where the ARM target component is unavailable but Rust sources
are installed, use `cargo +nightly build -Z build-std=core` with the same flags.

Select the actual connected device before flashing. Flashing replaces its
application; the build itself does not flash anything.

Connect any pair of supported radios. Both originate diagnostic probes at
slightly different intervals derived from their factory identifiers. Each valid
probe causes a directed reply. Replies do not cause further replies. The shared
test settings are channel 15, broadcast PAN 0xffff, extended source addresses,
and no requested MAC ACK. C6 uses 20 dBm and nRF52840 uses 8 dBm.

For this C6 RF investigation, TX power is fixed at **20 dBm**, including every
vendor-reference comparison. Lower-power sweeps and board-substitution trials
are already covered by the earlier work and must not be repeated as the next
diagnostic step. Similar RSSI across all three units and pairings directs the
investigation toward their shared setup; it is not evidence that the RF result
is satisfactory.

Look for `START kernel radio`, followed by `RADIO ... Statistics` lines. Both boards
must show increasing `received`, `replies_sent`, and `replies_received`; a
transmitted count alone establishes no peer reception. `transmit_errors` records
failed send attempts, `receive_errors` records rejected driver receives,
`ignored` records frames outside this diagnostic exchange, and `evicted` counts
oldest-frame eviction in the C6's fixed receive buffer.

## One-board run

The same image provides a useful check without a peer. Over roughly 30 seconds,
the printed tick count should advance, `receive_timeouts` should increase at the
probe interval, and `transmitted` should increase as the radio completes each
attempt. `interrupt_wakes` counts actual radio-driver wakeups from ISR context;
`notification_waits` counts entries into the kernel wait after a pending poll.
Their counts need not match: a completion can arrive before a wait is necessary.

`activity` names the operation attempted by the radio task, not a hardware status
register. It should generally show `Receiving` between the short transmissions.
A persistent `Transmitting` with no completed sends identifies a stuck TX path.
A persistent `Initializing` identifies startup that has not reached reception.
The reporter runs below the radio task's priority. C6 logging leaves interrupts
enabled so USB output cannot hold up the MAC interrupt's receive rearming.

Zero received frames and zero replies are expected without another Lattice
participant. This run can establish local progress and TX completion handling;
it cannot establish peer reception, receive IRQ handling, or RF range.

### Recorded C6 result — 2026-10-06

User-supplied serial output from C6 `E8:F6:0A:FB:EF:E0` reached tick 34,012:

| Measurement | Final value |
| --- | --- |
| Task activity at every reported sample | `Receiving` |
| Radio-reported completed transmissions | 15 |
| Radio interrupt wakeups | 15 |
| Receive deadlines | 15 |
| Task notification waits | 31 |
| Receive/transmit errors and evictions | 0 |
| Received frames and replies | 0 |

This chip's derived probe interval is 2,132 ticks. Fifteen completions by this
sample agree with that interval; the sixteenth probe is not due until at least
tick 34,112, plus initialization time. The receive deadlines are expected probe
triggers with no peer, not receive failures.

The capture demonstrates kernel startup and repeated TX interrupt → notification
→ task-resume cycles, returning to receive waits without a reported error or
reset during the capture. This one-board capture does not establish peer
reception, RX interrupt handling, replies, or RF range.

This capture predates moving the probe workload into `lattice-radio-test`. The
pair run below exercises the separated workload.

### Recorded C6 pair result — 2026-10-06

User-supplied excerpts from both terminals after the separated-image flash
instructions show bidirectional exchange. The connected C6s were
`E8:F6:0A:FB:EF:E0` and `9C:CC:01:40:67:98`; the pasted streams were not labeled
by port, so columns retain their order in the report.

| Measurement | First capture, final sample | Second capture, final sample |
| --- | --- | --- |
| Kernel tick | 41,040 | 66,050 |
| Received frames | 35 | 40 |
| Completed transmissions | 36 | 50 |
| Replies received | 17 | 20 |
| Replies sent | 18 | 20 |
| Radio interrupt wakeups | 71 | 90 |
| Receive deadlines | 18 | 30 |
| Notification waits | 90 | 121 |
| Ignored frames, receive/transmit errors, evictions | 0 | 0 |
| Reported task activity | `Receiving` | `Receiving` |

Both excerpts show increasing received frames and sent/received replies. At each
final sample, interrupt wakeups equal RX plus TX completions (35 + 36 = 71 and
40 + 50 = 90). Notification waits equal wakeups plus receive deadlines plus the
current receive wait. This supports the complete radio IRQ → kernel wakeup →
probe/reply workload → transmission path on both C6s.

The samples have different uptimes and no common wall-clock interval. Unanswered
probe totals cannot be classified as packet loss from these excerpts, and zero
driver errors do not prove loss-free delivery. RF range and Nordic execution or
cross-chip interoperability remain unverified.

## Probe diagnostics

The shared workload records these fixture-owned diagnostics:

- `probes_sent`: successfully completed probe transmissions, separate from replies.
- `pre_peer_probes`: probes sent before any valid peer traffic was observed.
- `pending_probe`: a probe whose reply window is still open.
- `unanswered`: a closed window with no matching reply, for a probe sent after
  observing peer traffic. A probe window closes when the next probe is attempted;
  it is counted once and does not trigger a retry.
- `unmatched_replies`: valid replies that do not match the open probe, including
  late or duplicate replies.
- `rssi_dbm`: most recent received-frame RSSI where the port supplies it (C6).
- `rx_abort_events` / `rx_abort_last`: observed hardware RX-abort interrupts and
  their latest port-native reason. For C6, reason 3 is CRC, 4 is invalid length,
  and 5 is filtering. Hardware abort masks are unchanged; this is not an exhaustive
  count of RF errors. `receive_errors` still counts errors returned by the receive
  API, which previously hid this distinction.

The [RF investigation](../../../experiments/esp32c6-rf/RF_RESULTS.md) retains
PHY comparisons, Wi-Fi/vendor controls, register snapshots, and antenna trials.
Those experiments are separate from the kernel transport checks.

## Build separation checks

`radio-probe` belongs only to the board fixture. It enables the optional shared
test crate, which enables the kernel's `test-support` entry. It includes both
originating and replying probe behavior. The ordinary kernel has neither.

Build the same board binary without `--features radio-probe` to check ordinary
startup. Use a separate output directory so the image intended for flashing is
still the probe build:

```sh
cargo +stable build --release --locked --bin radio --target-dir target/normal
cargo +stable tree --locked -e features
```

The ordinary dependency graph must have neither `lattice-radio-test` nor
`test-support`. Its ELF must have no `lattice_radio_test` or radio-testing symbols.
It initializes the radio and runs common peer discovery, including broadcasts
and directed presence responses. It does not answer `LVD` diagnostic probes.
The `peer-observer` fixture feature observes the normal handler without replacing
it; see [the discovery check](../peer/TESTING.md).

The portable codec tests run without an MCU target or kernel port:

```sh
cargo +stable test --locked --manifest-path src/kernel/radio/tests/Cargo.toml
```

Run that command from the repository root. The probe handler does not implement
network enrollment, discovery, surface exposure, or graph execution. Hardware
runs exercise radio IRQ → notification → resident task → transmission; no separate
scheduler-only test is required.
