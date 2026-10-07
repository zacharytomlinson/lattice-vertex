# Shared radio hardware test

This crate owns the diagnostic workload used by both board fixtures:

- `frame.rs`: probe/reply encoding and validation, plus wire-vector tests.
- `probe.rs`: traffic cadence and reply handling on the resident kernel task.
- `statistics.rs`: counters and activity reported by the board logger.
- `discovery.rs`: passive counters for ordinary kernel peer discovery; it provides
  no workload, radio settings, or packet handling.

The `runtime` feature is enabled by the board fixtures, which also select a
kernel port. Without it, the codec tests run on the host without depending on
the kernel. The kernel must never depend on this crate.

Hardware access goes through the kernel's explicitly gated `test-support` entry.
All actual driver operations, buffer ownership, ISR wakeups, and blocking waits
remain in the kernel and platform ports. The test supplies its workload through
that entry; ordinary appliance firmware supplies no radio loop or test callbacks.

See [the hardware instructions](../TESTING.md) for builds and recorded results.
The [discovery check](../../peer/TESTING.md) exercises the ordinary handler through
an observer rather than installing the RF-probe workload.
