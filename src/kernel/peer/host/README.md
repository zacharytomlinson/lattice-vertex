# Attached host participation

The optional `host-link` feature lets a local host use one ordinary resident peer.
The host and MCU share the MCU's radio identity. The kernel still owns channel
formation, reciprocal link maintenance, routing, deadlines, and radio operations.
An attachment cannot choose a PAN, channel, source identity, or network founder.

`kernel::host::submit`, `receive`, and `disconnect` operate from the owning transport
task. A single fixed mailbox accepts one request at a time, including its unread
response. Busy requests are rejected. No graph execution queue is added. Disconnect
invalidates the outstanding operation; it does not stop the resident peer or clear
its maintained neighbors. The radio task is notified by ordinary RTOS notifications.

The first supported operations are status and cursor-based `ReadLinks`. Local reads
use the actual maintained link table. Remote reads originate under the resident
address and use the existing bounded read paths, radio format, forwarding, and
response correlation. A reply ends at the local reader instead of being transmitted
to a fictitious USB neighbor. Radio read tokens advance across USB reconnects;
USB request tokens are only host correlation. Reads expire after eight seconds
without automatic retry. Link revisions identify an enumeration invalidated by
membership changes; zero peer marks its end.

`wire/` is the small, standalone `no_std` crate consumed by both the kernel and the
Linux client. It owns the local binary records and stream framing, not RF packet
encoding, hardware drivers, or kernel types. Version 1 uses little-endian integers,
COBS, and a zero delimiter. The longest current record is 46 encoded bytes,
including its delimiter, below one 64-byte USB CDC endpoint packet. Receivers still
handle arbitrary stream splitting/coalescing and discard oversized records through
the next delimiter. USB provides transport error detection; no extra checksum is
added. Tokens correlate responses, not authorization.

| Kind | Record after version/kind/u32 token |
| --- | --- |
| 0 Status | empty |
| 1 ReadLinks | u64 owner, u64 after, u64 via |
| 128 Ready | u64 address, u64 founder, u8 channel |
| 129 Link | u64 owner, u64 after, u64 via, u32 revision, u64 peer, u16 round-trip milliseconds |
| 255 Error | u8 reason: busy, invalid request, transmit failure, timeout |

The XIAO USB application and Linux client belong to `lattice-tablet`, under
`firmware/nrf52840` and `peer`. The port's diagnostic console remains a test fixture;
its text is not this interface. Surface reads, state reads, invocation, and delta
delivery have not yet been exposed over the host link.

Checks:

```sh
cargo +stable test --manifest-path src/kernel/peer/host/wire/Cargo.toml --locked
cargo +stable test --lib kernel::peer --no-default-features --features port-test,host-link -- --test-threads=1
```
