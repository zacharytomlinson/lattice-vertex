# Shared flash storage

`lattice-storage` owns bounded flash-region access, inactive image staging and
committed local configuration. Ports retain ownership of the physical flash
peripheral and expose an exclusive borrowed region. It has no RTOS, networking,
appliance callbacks, filesystem or heap. This permits the separately linked Nordic
loader to use image handling without linking the kernel runtime.

## Local configuration

`configuration::Configuration` opens a protected `region::Region` supplied by the
port. It needs no update key or release policy. The port prevents image staging,
transfer journals and boot swap from reaching the region. Unconfirmed trial
firmware receives a read-only view, preserving rollback-compatible data until
confirmation. A caller must serialize operations through the port's flash owner.

Two equal erase-aligned banks retain the current and next copy. With the current
16 KiB reservations, each bank is 8 KiB and holds at most **8,140 payload bytes**.
The payload is local configuration, not a firmware image or a log of unfinished
operations. The [program loader](../program/README.md) owns decoding, native
binding reconstruction and installation.

| Bank section | Bytes |
| --- | --- |
| Header | `LCF` version 1, generation `u64`, payload length `u32` (16 total) |
| Payload | Declared bytes; final word padded with erased bytes |
| Footer at bank end | SHA-256 of header plus unpadded payload, then a four-byte commit word (36 total) |

`open` validates both committed copies and selects the higher generation. It never
combines bytes from different copies. Blank/incomplete first writes produce no
configuration; corrupt committed data with no valid fallback is an error. Equal
committed generations are ambiguous and rejected. Generations never wrap.

`begin` selects the inactive bank. `erase_next_page` performs one erase, allowing
the kernel to yield between pauses. `write` accepts sequential chunks of at most
256 bytes and checks repeated ranges without rewriting them. `commit` verifies
physical header/payload bytes, writes/checks the digest, and programs the final
zero commit word last. A failure during that final operation is uncertain: reopen
to establish which copy committed before beginning another replacement.

Reading does not erase data. Saved bytes stay intact across image replacement.
Configuration compatibility, program validation, binding reconstruction and
enablement belong to the kernel loader, which is not implemented by this storage
component. The caller must also schedule flash pauses; `commit` verifies the
bounded copy synchronously and is not an ISR operation.

## Inactive firmware images

`lattice-storage` owns the bounded staging mechanics shared by the Nordic
and C6 update ports: sequential chunk acceptance, page erase progress, duplicate
comparison, final-word padding, failed-write rejection, and SHA-256 verification.
It has no allocator or image-sized buffer. Work uses a 256-byte aligned buffer;
the candidate record holds bounds, volatile write progress, and incremental
SHA-256 state (or the completed digest).

The `kernel::update` service
owns signed packages, persistent page checkpoints, and local activation. This
crate resumes only from a caller-validated durable offset; it never interprets
network packets or trusts a remote offset itself. Admission and activation policy
remain in the parent updater.

The port owns the inactive reservation and the decision to change boot metadata.
Nordic erases the full active-sized candidate area because its loader swaps the
whole slot. C6 erases only the candidate's sectors and boots that slot directly.
Both ports reject staging during an unconfirmed trial so the rollback image stays
available. Signature authorization remains distinct from a matching byte digest.

The port integration checks exercise this component through their real image
handling paths: [Nordic](../../port/nrf52840/boot/src/tests.rs) and
[C6](../../port/esp32c6/boot/src/tests.rs). Flash geometry and address ranges
are checked before erase or write. A durable page boundary resumes the written
prefix while requiring the remaining erase pages to be prepared again.


`verify_step` reads at most the requested byte count and reports completion only
after the entire digest matches. Resuming or modifying the candidate invalidates
its hash state. Ports recheck their small image prefix before readiness/selection.
A verified unchanged region reuses its digest at selection; there is no second
full-image read hidden inside that transition. The synchronous `verify` convenience
is reserved for direct local callers; common staging uses bounded steps.
