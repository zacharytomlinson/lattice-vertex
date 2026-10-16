# Attached host participation

The optional `host-link` feature lets a local host use one ordinary resident peer.
Host and MCU share the MCU's radio identity. The kernel owns formation, maintained
links, routing, deadlines, and radio operations. The attachment cannot select a
PAN, channel, source identity, or network founder.

`kernel::host::submit`, `receive`, and `disconnect` operate from the owning transport
task. One fixed mailbox accepts one request at a time, including its unread reply.
Busy requests are rejected. Disconnect invalidates host completions and cancels
local return paths without withdrawing the resident peer. It cannot undo a native
operation that has already reached its owner. No graph execution queue is added.

The implemented operations are status, cursor-based link reads, immutable surface
ranges, current-state reads, and native transformer invocation. Local requests use
the existing description/native binding. Remote requests use the ordinary radio
messages, bounded read/call paths, duplicate handling, and route learning. Replies
terminate at the local caller. Host tokens are not radio neighbors or credentials.

Reads seed their 16-bit radio counter once from the first host read token, then
advance it across kinds and USB reconnects. This avoids deliberately reusing
read 1 after an MCU reboot, while retaining probabilistic collision limits. Calls
carry the host's 32-bit token as their invocation identity. The Linux client seeds
its token sequence from OS entropy on each open; reconnecting or rebooting the MCU
does not deliberately restart call numbering at one. This is probabilistic identity
within bounded, volatile duplicate history, not durable exactly-once execution.
Requests time out after eight seconds without automatic retry. A call timeout,
USB loss, or failed transmission does not prove the native operation did not run.

`Invoked` means the function returned success. The result does not contain invented
post-state. An explicit current-state read returns the established value and change
watermark, `UnknownAttribute`, or `Busy`. A valid value may itself be unavailable.
Native argument errors, unknown handles, appliance errors, and duplicate conflicts
remain distinct. The kernel does not interpret IR preconditions; native functions
still enforce their appliance's hardware limits.

## Wire ownership

[`../wire`](../wire) is the standalone `no_std` crate `lattice-peer-wire`. Its scalar
codec is shared by radio frames, surface literals, and USB. `host` is an optional
module containing the local records/stream decoder; ordinary appliance builds do
not include it. Neither module contains a radio driver, RTOS, allocator, or IR
interpreter. The radio remains **LVT v6**, and surfaces remain **LSF v1**.

Host version **2** uses little-endian records, COBS, and a zero delimiter. Version 1
is rejected. The largest current encoded record is 87 bytes, in a fixed 128-byte
buffer. USB CDC may split/coalesce records and needs a terminating short packet
(or ZLP at an exact endpoint boundary). This does not fragment radio messages:
each surface range, state read, invocation, and result remains one RF frame.

All records begin with version (`u8`), kind (`u8`), and token (`u32`).

| Kind | Remaining fields |
| --- | --- |
| 0 Status | empty |
| 1 ReadLinks | owner `u64`, after `u64`, via `u64` |
| 2 ReadSurface | owner `u64`, offset `u16` |
| 3 ReadState | owner `u64`, attribute `u64` |
| 4 Invoke | owner `u64`, transformer `u64`, scalar argument list |
| 128 Ready | address `u64`, founder `u64`, channel `u8` |
| 129 Link | owner/after/via `u64`, revision `u32`, peer `u64`, round-trip milliseconds `u16` |
| 130 Surface | owner `u64`, offset/total `u16`, up to 67 bytes |
| 131 State | owner/attribute `u64`, result tag; values add sequence `u32` and typed scalar or 255 unavailable |
| 132 Invoked | owner/transformer `u64`, native status `u8` |
| 255 Error | reason `u8`: busy, invalid request, transmit failure, timeout |

The argument section has the same 61-byte/30-value radio limit. It starts with a
count byte and declared-width scalars. State tags are 0 value, 1 unknown attribute,
2 busy. Native statuses are documented in [INVOCATION.md](../INVOCATION.md).

The XIAO USB application and Linux consumer live in `lattice-tablet`. The consumer
assembles only explicitly requested surfaces and checks completeness before
inspection/saving. The MCU retains no foreign surface or global registry. Delta
streaming to the host, continuous daemon operation, and UI integration are later
work; read-after-call validation is not a substitute for that event path.

Checks:

```sh
cargo +stable test --manifest-path src/kernel/peer/wire/Cargo.toml --features host --locked
cargo +stable test --lib kernel::peer --no-default-features --features port-test,host-link -- --test-threads=1
```

Maintained tests cover local/remote reads, guided traversal, exact response
correlation, reconnect cancellation, typed arguments, native error distinctions,
duplicate result replay, and a lost call result without re-execution. Hardware
validation uses the Nordic `test-appliance` fixture; its dimmer is software-backed.
