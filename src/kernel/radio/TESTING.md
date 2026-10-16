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
layout/packaging. The XIAO path in the
[Nordic README](../../port/nrf52840/tests/README.md) supplies that layout and a
USB console; select `radio-probe,xiao-bootloader,usb-console` for this workload.
Where the ARM target component is unavailable but Rust sources
are installed, use `cargo +nightly build -Z build-std=core` with the same flags.

Select the actual connected device before flashing. Flashing replaces its
application; the build itself does not flash anything.

Connect any pair of supported radios. Both originate diagnostic probes at
slightly different intervals derived from their factory identifiers. Each valid
probe causes a directed reply. Replies do not cause further replies. The shared
test settings are channel 15, broadcast PAN 0xffff, extended source addresses,
and no requested MAC ACK. C6 uses 20 dBm and nRF52840 uses 8 dBm.

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

Historical RF measurements, vendor comparisons, and one-off captures are kept
outside the kernel repository. The maintained probe verifies the transport/IRQ
path; its counters do not prove radio range or loss-free delivery.

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


## Receive turns in the resident peer

Ordinary peer startup enables a receive opportunity after each successful TX.
It is independent of pending local deltas and returns the first valid captured
frame through the ordinary receive path. The maximum wait accounts for a full
250-kbit/s frame, turnaround, and tick quantization; at 1 kHz it is six ticks.
An in-progress PHY frame gets a bounded completion interval before cancellation
for a deadline or new local work. Four owned frame records preserve captures across consecutive send calls, with
oldest eviction under pressure. Addressed peer frames request MAC ACKs and have
at most four identical transmission attempts with bounded randomized backoff.
Broadcasts remain single attempts. Native execution is not separately retried.
Raw probe workloads and bootstrap retain their own receive windows.

Portable checks beside `mac.rs` cover ACK target/sequence validation, duplicate
suppression after lost ACKs, distinct senders, DSN/timer wrap, bounded history
expiry/eviction, and retry timing bounds. The frame codec checks ACK requests are
limited to unicast. Nordic immediate ACK turnaround belongs to its radio IRQ;
C6 uses its hardware MAC. Hardware captures and deliberate loss experiments stay
in the external lab.

Portable checks beside `receive.rs` cover owned-byte retention, FIFO/eviction,
channel reset, tick rounding, in-progress frame completion across tick wrap, and
the cap on a stuck receiver. C6 and Nordic
builds check the common integration. Hardware validation must use ordinary peers,
not assume that a successful spaced probe covers back-to-back state changes.


A two-Nordic A/B/A isolated the deterministic paired-delta failure: the source
completed all 16 delta transmissions, but only eight reached the attached host.
Disabling only the companion's delta rebroadcast delivered 15/16; restoring it
restored the repeated second-delta loss. With receive turns and bounded completion
of active frames, an uninstrumented 32-call run delivered 57 of 58 registered
changes, 29/32 native results, and 28/30 surface ranges. The owner watermark was
read before/after to count generated changes independently of command results.
No mutation or packet was automatically retried. Residual losses and absolute
RSSI/range remain unresolved; this run is not a reliability guarantee. Diagnostic
images, traces, and the rebroadcast bypass remain exclusively in the external lab.
