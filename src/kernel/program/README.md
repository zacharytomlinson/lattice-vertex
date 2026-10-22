# Resident program loading and wiring

This component receives previously unknown local program definitions, validates
them, commits them to protected storage, and attaches their source relationships
to the appliance's owned states. Startup rebuilds those links from committed
definitions against the current native bindings. Firmware supplies no automation
callback, installation loop, storage policy, or message handler.

**Installed means locally committed, validated and wired, but inactive.** This
implementation does not yet admit upstream activations, execute primitive records,
carry traversal values, or enable a distributed program. It also does not install
new retained-state maintenance or time/solar resources. Unknown record kinds are
rejected. The agreed evaluation-on-enable/recovery behavior belongs to the next
execution integration; it is not inferred from an installation receipt.

## Ownership and publication

`kernel::storage` owns the physical port storage. Its configuration region is
borrowed by this loader or the optional signed updater, never duplicated through
another flash-peripheral handle. `start_with_storage` supplies that hardware
binding without a release key or signing policy. `start_with_updates` uses the
same owner and additionally enables signed firmware updates. A peer started
without storage still forwards program traffic and reports `Unsupported` locally.

The loader runs in the resident radio/peer task. It reconstructs before PHY
formation, using the task stack rather than the board entry stack. Trial firmware
cannot confirm startup if its saved program definitions fail reconstruction.
Confirmed firmware can still expose a reconstruction error for recovery without
inventing a physical mutation. Nothing invokes a native getter or transformer
merely to validate/install its declaration.

The resident definition arena is 8 KiB. Actual admission also respects the port's
configuration capacity (8,140 bytes in layout 3). Records occupy variable byte
extents; there is no fixed array slot per edge or StateId-to-ProgramId registry.
One incomplete installation owns the remaining tail while it receives bytes.
Uncommitted receiving reservations expire after 30 seconds without an accepted
Begin/Data request. Once persistence begins, ordinary service continues between
flash steps; a timeout/disconnect does not cancel a committed definition.

Each owned state contains its local dependency-list head. A source record has a
native reference, a local program entry, and its next-sibling word. After commit,
the kernel publishes the links. At restart it clears and rebuilds all heads after
validating every committed definition. Sibling words are kernel-maintained RAM
links; serialized/exported definitions contain `0xffff` in those words. They are
not DAG successors, remote addresses, or caller-supplied pointers.

Failed validation writes no flash and publishes no links. A flash failure can
leave the commit outcome uncertain; the service stops further writes until
restart establishes which copy committed. Incompatible definitions remain saved
and readable, but are not attached to native states. Existing native reads/calls
remain separate from program installation.

## Definition format: LPD v1

Integers are little-endian. No Rust layout is copied. All offsets below are byte
offsets, and every record reference must name an actual record boundary.

| Header offset | Field |
| --- | --- |
| 0 | `LPD`, version byte 1 |
| 4 | Declared carried-value byte capacity, `u16` |
| 6 | Declared local scratch byte capacity, `u16` |
| 8 | Source count, `u16` |
| 10 | Local head entry offset, `u16`; `0xffff` means this owner has no head |
| 12 | First operation-record offset, `u16` |
| 14 | Reserved `u16`, zero |
| 16 | Expected 32-byte native surface fingerprint |

Sources follow the 48-byte header. Each occupies 12 bytes: durable native state
reference (`u64`), local entry offset (`u16`), and sibling word (`u16`, `0xffff` on
input). A source appears at most once within a definition. The authoring peer must
avoid duplicate source-to-program attachments across separately installed portions
of the same distributed program.

Each operation record begins with its complete byte length (`u16`) and kind
(`u8`). Field layouts follow:

| Kind | Fields |
| --- | --- |
| 0 entry | Upstream link, input-location list, next link |
| 1 read | Native state reference `u64`, output location, next link |
| 2 unary | Unary primitive, operand, output location, next link |
| 3 binary | Binary primitive, left/right operands, output location, next link |
| 4 test | Comparison primitive, left/right operands, true/false links |
| 5 branch | Boolean operand, true/false links |
| 6 invoke | Native transformer reference `u64`, argument list, success/failure links |

The invoke record describes continuation on the returned native result. It does
not infer physical completion or encode an acceptance-only continuation. A
physical completion dependency belongs to an observed state. Execution of these
records is not implemented by the loader.

Lists start with their `u8` count. Operands are a tag followed by their data:
constant (0: existing typed scalar encoding), carried location (1: scalar type
and `u16` offset), or scratch location (2: scalar type and `u16` offset). An output
must be a location. Entry inputs must be disjoint carried locations; a head has
none. Scalar and primitive tags match the existing
[surface vocabulary](../peer/EXPOSURE.md#expression-vocabulary).

Links are stop (tag 0), local (tag 1 plus `u16` record offset), or foreign (tag 2
plus owner `u64`, definition offset `u16`, generation `u32`, record offset `u16`).
The foreign definition offset identifies its stored prefix; its record offset
is relative to the LPD definition after that prefix. A source's entry must lead
upstream unless it is the head. An action-only entry with no sources needs no
upstream link: functions are not observation dependencies.

Local successors point forward in record order and cannot enter another entry.
Local upstream links point to earlier entries. The authoring peer lays out the
complete distributed DAG and validates foreign interfaces. The receiver checks
native types/signatures, local link bounds, reachability, arithmetic domains,
location bounds and initialization on every incoming local path. Branch choice
represents `&&`/`||`; neither needs an accumulator or a dedicated opcode.

Validation permits at most 128 local records, using bounded temporary analysis
storage. Carried/scratch capacities describe the compiled layout; this loader
does not allocate traversal contexts or promise execution capacity. That admission
belongs to the execution layer. Firmware callbacks cannot substitute for missing
record kinds.

## Persistent image

The committed payload starts with `LPG`, version byte 1, followed by local
definitions. Each has a 24-byte prefix: total prefix-plus-definition length
(`u16`), reserved zero (`u16`), generation (`u32`), installing caller (`u64`),
transfer nonce (`u32`), and four reserved zero bytes. Definitions append to this
image; there is no deletion, live replacement or reuse of occupied extents yet.
Generations do not wrap. The immutable caller/nonce and canonical definition
digest distinguish a repeated commit from conflicting work after a lost reply or
restart. A reservation receipt is not a commit receipt.

The [shared store](../storage/README.md#local-configuration) provides interrupted-
write recovery. This layer determines what those bytes mean and when their native
links can become visible. A compatible address-only firmware relocation preserves
the surface fingerprint and resolves the same references to the current handles.

## Installation transport

LVT v7 adds program request/result opcodes **21/22**. Retired callback opcodes
19/20 remain reserved. Program exchanges share the existing eight reverse paths,
read-token namespace, owner routes and hop limit. They add no route catalog or
special tablet/nucleus role. The envelope is founder `u64`, channel `u8`, reader
`u64`, owner `u64`, request `u16`, scope `u8`, then the command/reply. Results append
the ordinary path-cost field. Host v4 carries these through kinds **7/0x87**.

| Command | Payload after command byte |
| --- | --- |
| 0 info | None |
| 1 begin | Nonzero transfer nonce `u32`, definition byte length `u16` |
| 2 data | Transfer, offset `u16`, length `u8`, 1–48 bytes |
| 3 commit | Transfer, SHA-256 of the complete canonical LPD definition |
| 4 abort | Transfer; only a receiving reservation can be discarded |
| 5 read | Offset `u16` within the committed local image |
| 6 dependencies | Native state reference `u64`, source cursor `u16`; `0xffff` starts at the head |

Replies contain status `u8`, offset `u16`, generation `u32`, accepted `u16`, total
`u16`, byte count `u8`, and up to 48 bytes. Begin/Data/Commit offsets identify the
reserved definition; accepted/total count its LPD bytes. Info reports committed
image length and, while receiving, the current progress; its data contains total
capacity and maximum record count as two `u16`s. Read reports the requested offset
and complete image length. Dependency replies identify the installed definition
and carry current cursor, next cursor and entry offset (`u16` each); no data means
end of list. They enumerate the actual reconstructed native dependency chain.

Receiving, persisting and installed are distinct statuses. Other results report
busy, unsupported, invalid, bounds, binding, compatibility, storage or conflict.
Data ranges are ordered; an identical duplicate is acknowledged without another
copy. Conflicting bytes/digests fail. Persisting/installed receipts are idempotent
for the same caller/nonce/definition. An abandoned incomplete reservation cannot
occupy installation capacity indefinitely. This is structural admission and
transport integrity, not a new authentication or FSPS proof mechanism.

## Maintained checks

Host checks enforce declaration bounds, typing, initialization, publication after
commit, duplicate/conflict behavior, reservation expiry and reconstruction against
fresh native storage. Shared transport checks include the new message family.
Both ports' existing radio fixture has a `program-test` feature that starts with
storage and an ordinary test appliance without a signing policy. It contains no
installed automation. Build with `--bin radio --features program-test`; the port's
`update.py prepare --program` packages its loader/application artifacts under
`tests/target/program`. Preparation does not flash a board. Follow the port's
existing layout-3 provisioning procedure with those artifacts before a physical
test. This mode needs no release public key. Signed-update builds use
the same service. Build/host checks do not establish a live radio installation or
program execution.
