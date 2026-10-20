# Shared radio hardware test

This crate owns the diagnostic workload used by both board fixtures:

- `frame.rs`: probe/reply encoding and validation, plus wire-vector tests.
- `probe.rs`: traffic cadence and reply handling on the resident kernel task.
- `statistics.rs`: counters and activity reported by the board logger.
- `discovery.rs`: passive counters for ordinary kernel peer discovery; it provides
  no workload, radio settings, or packet handling.
- `surface.rs`: initialized switch/dimmer native appliances and passive range/checksum records
  for owner-directed surface reads. It emits no packets and retains no foreign
  mathematical surface or complete-transfer state. The dimmer locally installs
  four level observations and a signed newest-minus-previous result through
  `Appliance::install_state`; the kernel owns their maintenance and deltas.
  The native `SetLevel` still reports only its physical power/level changes.

The `runtime` feature is enabled by the board fixtures, which also select a
kernel port. Without it, the codec tests run on the host without depending on
the kernel. The kernel must never depend on this crate.

Hardware access goes through the kernel's explicitly gated `test-support` entry.
All actual driver operations, buffer ownership, ISR wakeups, and blocking waits
remain in the kernel and platform ports. The test supplies its workload through
that entry; ordinary appliance firmware supplies no radio loop or test callbacks.

See [the hardware instructions](../TESTING.md) for builds and counter interpretation.
The [discovery check](../../peer/TESTING.md) exercises the ordinary handler through
an observer rather than installing the RF-probe workload.

These checks are reusable across the supported ports. Receive-only RF experiments,
per-token capture callbacks, register sampling, and flash recorders belong in an
external lab and are not optional features of this crate.
