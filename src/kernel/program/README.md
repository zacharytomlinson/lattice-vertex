# Resident programs

The kernel retains each addressed connection's activation and required values in
RAM. A local appliance/time change resumes that connection directly; an accepted
upstream update supplies its current input. No activation walks back to the head.
Local predicates, branches, arithmetic and native calls still use the existing
LPD v1 records. LLVM-generated native computation is the next backend, not an
implemented capability. See the [implementation plan](../../../docs/PROGRAM_DESIGN.md).

This component validates incoming definitions, commits them to protected storage,
and attaches their sources to owned appliance states and time deadlines. Startup
reconstructs those bindings. Firmware supplies no automation callback, installation
loop, storage policy or message handler.

**Installed means committed, validated and wired, but inactive.** Durable head
admission enables the circuit. Incoming activation starts LOW. Only enabled heads
originate initial evaluation; other entries need an incoming HIGH. Prior activation
and carried values are never restored after reboot. Authored mutations keep their
meaning: LOW closes a connection; it does not invent a resting mutation. Native
calls execute as bounded immediate operations; subsequent device work is scheduled separately. Automation-authored history is
still unsupported.

## Ownership and publication

`kernel::storage` owns the physical port storage. Its configuration region is
borrowed by this loader or the optional signed updater, never duplicated through
another flash-peripheral handle. `start_with_storage` supplies that hardware
binding without a release key or signing policy. `start_with_updates` uses the
same owner and additionally enables signed firmware updates. A peer started
without storage still forwards program traffic and reports `Unsupported` locally.

The loader runs in the resident radio/peer task. It reconstructs before PHY
formation, using the task stack rather than the board entry stack. Trial firmware
cannot confirm startup until its definitions reconstruct. Configuration remains
read-only during an unconfirmed trial. After confirmation permits writes, one
configuration checkpoint commits a new receiver incarnation before execution is
admitted. Its epoch distinguishes signal sequences issued before restart. This is not a
mutation journal.
Confirmed firmware can still expose a reconstruction error for recovery without
inventing a physical mutation. Nothing invokes a native getter or transformer
merely to validate/install its declaration.

The resident definition arena is 8 KiB. Actual admission also respects the port's
configuration capacity (8,140 bytes in layout 3). Records occupy variable byte
extents; there is no fixed array slot per edge or StateId-to-ProgramId registry.
One incomplete installation owns a fixed **512-byte** incoming buffer and reserves
the largest available arena extent. The limit covers the complete incoming LPD,
including its header, sources and operations. Begin rejects larger fragments with
Bounds before accepting data. This is a deliberate initial fragment limit; increase
it only for a demonstrated need. The complete LPD is validated before matching
computation locally; only its connection record may need resident space. The
512-byte staging capacity is independent of the 8 KiB resident arena.
Uncommitted receiving reservations expire after 30 seconds without an accepted
Begin/Data request. Once persistence begins, ordinary service continues between
flash steps; a timeout/disconnect does not cancel a committed definition.

If a concrete fragment later exceeds this limit, the planned extension is an
installation session that stages complete components incrementally. Each completed
component is checked and stored provisionally, then its receive workspace is reused.
Provisional records remain outside live dependency lists and cannot execute. Session
completion checks cross-references and whole-fragment validity by walking staged
records, then commits and publishes the fragment. Failure discards the session's
additions, including new connections to shared computation; existing shared users
remain intact. Receive/validation working memory stays bounded independently of
total fragment size; provisional storage still consumes capacity. This extension
is not implemented: today 512 bytes caps the complete incoming fragment.

Each owned state contains its local dependency-list head. A source record has a
native reference, a local program entry, and its next-sibling word. After commit,
the kernel publishes the links. At restart it clears and rebuilds all heads after
validating every committed definition. Sibling words are kernel-maintained RAM
links; serialized/exported definitions contain `0xffff` in those words. They are
not DAG successors, remote addresses, or caller-supplied pointers.

Failed validation writes no flash and publishes no links. A flash failure can
leave the commit outcome uncertain; the service stops further writes until
restart establishes which copy committed. Incompatible definitions remain saved
and readable, but are not attached to native states. Their Compatibility/Binding
status is separate from valid configuration storage: settings may still restore and
commit without rewriting those definitions. Existing native reads/calls remain
separate from program installation.

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
| 7 temporal | Activation entry `u16`, temporal predicate, true/false links |

A temporal predicate is tag 0 plus start/end `u32` local seconds, or tag 1 plus
two solar boundaries. Each solar boundary is event `u8` (sunrise=0, noon=1,
sunset=2) and signed offset seconds `i32`. The record registers a local source even
when a preceding condition short-circuits. Its activation entry must precede and
reach it, and lead upstream to a head/foreign owner. It consumes no carried slot.
The [time component](../time/README.md) owns its clock/deadline semantics.

The invoke record describes continuation on the returned native result. It does
not infer physical completion or encode an acceptance-only continuation. A
physical completion dependency belongs to an observed state. The bounded native operation returns before that evaluation follows either edge;
other evaluations, state-event processing and radio service can continue meanwhile.

Lists start with their `u8` count. Operands are a tag followed by their data:
constant (0: existing typed scalar encoding), carried location (1: scalar type
and `u16` offset), or scratch location (2: scalar type and `u16` offset). An output
must be a location. Entry inputs must be disjoint carried locations; a head has
none. Scalar and primitive tags match the existing
[surface vocabulary](../peer/EXPOSURE.md#expression-vocabulary).

Links are stop (tag 0), local (tag 1 plus `u16` record offset), or foreign (tag 2
plus owner `u64`, definition offset `u16`, generation `u32`, record offset `u16`).
The foreign definition offset identifies its stored prefix; its record offset
is relative to the LPD definition after that prefix. LPD v1 still validates a source entry's authored upstream reference unless it is
the head; the runtime no longer follows that reference for activation. An action-only entry with no sources needs no
upstream link: functions are not observation dependencies.

Local successors point forward in record order and cannot enter another entry.
Local upstream links point to earlier entries. The authoring peer lays out the
complete distributed DAG and validates foreign interfaces. The receiver checks
native types/signatures, local link bounds, reachability, arithmetic domains,
location bounds and initialization on every incoming local path. Branch choice
represents `&&`/`||`; neither needs an accumulator or a dedicated opcode.

Validation permits at most 128 local records, using bounded temporary analysis
storage. Carried/scratch capacities describe the compiled layout; this loader
does not allocate execution contexts. Enablement and incoming updates check execution
capacity separately. Firmware callbacks cannot substitute for missing record kinds.

## Persistent image

The committed payload starts with `LPG`, version byte **5**. Every extent begins
with length `u16`, flags `u8`, kind `u8`. A free extent (kind 4) has only this
four-byte header; occupied extents retain the existing 24-byte prefix: generation
`u32`, installing caller `u64`, transfer nonce `u32`, admission revision `u32`.
Flags bit 0 enables a head. Bits 6–7 count 0–3 trailing padding bytes, allowing a
whole free extent to be consumed without leaving an unusable fragment. RAM-only
initial-evaluation (bit 1) and staged-install (bit 3) flags are never serialized.

| Kind | Contents after the occupied prefix |
| --- | --- |
| 0 inline | Complete LPD v1; its computation and first connection set |
| 1 clock | Commissioned profile described below |
| 2 connection | Reference to a computation plus this connection set's source/head/link records |
| 3 body | Original LPD template retained after its first connection was removed; never active itself |
| 4 free | No occupied prefix or executable contents |
| 5 setting | Durable state binding, type and optional last committed scalar; private or exposed |

A setting record is 36 bytes. Its first four bytes use the extent header; bytes
4–11 carry the durable binding, byte 12 indicates availability, bytes 13–23 are zero,
and bytes 24 onward use the shared typed-scalar encoding (type only when unavailable).
Unused bytes are zero. Settings are not fragments; normal definition traversal skips
them. The existing committed-copy mechanism owns their durability.

Matching compares the surface fingerprint, carried/scratch layout, and ordered
operation payloads, including types, constants and native references. Connections,
record positions, source subscriptions and head designation are not computation.
A kind-2 record stores body offset `u16`, body generation `u32`, selected head
`u16`, original LPD length `u16`, source count `u16`, then its 12-byte source records.
For each operation it stores the original record offset/length (`u16` each), the
temporal activation entry if applicable, and that operation's links in LPD encoding.
The body reference always points directly to kind 0 or 3, never another connection.
The evaluation view combines those links with the shared operands; it cannot select
a different connection's destination. Virtual LPD offsets remain exactly as authored.

LPG v1/v2/v3/v4 inline definitions remain readable. The startup checkpoint upgrades
the header without relocating live definitions or changing generations. V1 starts
disabled; later admission state is preserved. Removing one connection keeps the
body while another still references it. Removing the last frees it. Adjacent free
extents coalesce and an unused tail is trimmed; live extents never move.

Generations derive from the monotonically increasing committed-image generation
and do not wrap. Reusing an address cannot revive an old continuation or deletion.
Caller/nonce plus the reconstructed canonical LPD digest still distinguish retry
from conflicting installation. Read returns Busy during persistence rather than
exposing an uncommitted reused extent. A reservation is not a commit receipt.

One kind-1 context record uses the same 24-byte prefix followed by a 32-byte clock
profile. Its length is 56, generation is the profile revision, and other prefix
fields are zero. It appends once and is subsequently replaced in place within a
new committed image. Definitions before or after it keep their original offsets.
The exported image includes this context; it is not an LPD definition.

The [shared store](../storage/README.md#local-configuration) provides interrupted-
write recovery. This layer determines what those bytes mean and when their native
links can become visible. A compatible address-only firmware relocation preserves
the surface fingerprint and resolves the same references to the current handles.

## Installation transport

LVT v8 adds program request/result opcodes **21/22**. Retired callback opcodes
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
| 29 remove | Local connection offset `u16`, generation `u32` |

Clock commands 15–18 share this service; layouts and revision semantics are in the
[time component](../time/README.md#commissioning-and-reacquisition).

Replies contain status `u8`, offset `u16`, generation `u32`, accepted `u16`, total
`u16`, byte count `u8`, and up to 48 bytes. Begin/Data/Commit offsets identify the
reserved definition; accepted/total count its LPD bytes. Info reports committed
image length and, while receiving, the current progress; its data contains total
image capacity, maximum record count and maximum incoming fragment bytes as three
`u16`s, in that order. The fragment limit is appended to the earlier two fields.
Read reports the requested offset
and complete image length. Dependency replies identify the installed definition
and carry current cursor, next cursor and entry offset (`u16` each); no data means
end of list. They enumerate the actual reconstructed native dependency chain.

Receiving, persisting and installed are distinct statuses. Other results report
busy, unsupported, invalid, bounds, binding, compatibility, storage or conflict.
Execution adds Enabled (12), Disabled (13), Accepted (14), Stale (15), and Unavailable
(16), and Removed (17); an Accepted signal receipt does not claim that its native operation has finished.
Installation Data ranges may arrive out of order through the common Unique + Complete
channel. A 64-byte coverage map tracks the existing 512-byte staging buffer; progress
reports its contiguous accepted prefix and Commit still requires the full definition
and matching digest. An already-covered range is acknowledged without another copy.
Issued transmission contents are immutable. Persisting/installed receipts are idempotent
for the same caller/nonce/definition. An abandoned incomplete reservation cannot
occupy installation capacity indefinitely. This is structural admission and
transport integrity, not a new authentication or FSPS proof mechanism.

## Explicit removal

Remove addresses one local connection set, including one stored inline with its
computation. The handler detaches its state sources, temporal alarms, pending
activations and retained incoming contexts. Any previously selected downstream
output is withdrawn through an addressed LOW update. Pending source cursors advance to surviving
siblings before the old storage can be reused. Ready native calls are canceled;
already-running native calls finish with their continuation detached. Their
the bounded operation has already returned before another graph command is handled.
Other connection sets, native states/functions and commissioned time remain intact.

The reply echoes offset/generation. Persisting means local removal took effect;
Removed means the reclaimed image committed, or that generation is already absent.
Retry with the same identity after a timeout. A newer occupant at the same offset
is untouched. Busy means a different configuration operation owns persistence.
A power loss before commit can restore the old image; an unconfirmed deletion must
be retried. No success is reported merely because RAM was reclaimed.

Earlier ready local evaluations run FIFO before a later command. Remote handoffs
and native calls yield independently; deletion does not wait for the entire graph.
Traffic already beyond the vertex is not recalled. Removing an entire distributed
path requires deletion commands to its participating owners; this operation adds
no global automation registry, deletion crawler, or replacement/drain protocol.

## Admission and execution

The same LVT program route and host envelope carry admission and addressed signal
updates. Relays route to the named owner through existing peer paths. Neither the
tablet nor a compiler participates in ongoing evaluation.

| Command | Fields after command byte |
| --- | --- |
| 7 admission state | Definition offset `u16`, generation `u32` |
| 8 set admission | Same reference, expected revision `u32`, enabled `u8` |
| 9 receiver session | None |
| 14 execution statistics | None |
| 30 signal | Source Entry, target Entry, sender epoch `u64`, sequence `u32`, active `u8`, offset `u16`, 0–24 carried bytes |

An Entry is definition offset `u16`, generation `u32`, record offset `u16`.
The source owner comes from the addressed envelope. Old execution tags 10–13
(reserve/data/commit/release visit) are rejected. LPD v1 and persistent LPG v5 have
not changed; there is no second logic language or legacy execution runtime.

Admission remains revision-checked and durable. Enablement schedules the head
once from current inputs; disablement withdraws its selected output. Repeating an
accepted command does not generate a new edge. Recovery evaluates enabled heads
once after local observations become available. Downstream entries remain LOW
until upstream supplies activation. A local enable receipt does not establish
remote readiness.

### Retained inputs and local evaluation

`execution.rs` owns input bindings, local evaluation work, selected outputs and
native-call argument records. `evaluation.rs` retains the existing declared-width
arithmetic and typed value access. `runtime.rs` connects this engine to the peer,
source notifications, clock deadlines, storage and immediate native results.

Incoming bindings are scoped by source owner/source Entry/target Entry. Each uses
Unique + Temporal acceptance within its sender incarnation. Equal or older updates
are acknowledged without evaluation. Accepted input sequences persist with that
binding in RAM, independently of the expiring observation-forwarding cache.
Only a complete, typed update changes the input. Missing scalars are unavailable,
not fabricated zeroes. A local reevaluation uses the current retained input and
fresh owned state; it does not ask the head to run again.

Separate installed connection sets remain independent even when they share code.
Where authored paths converge on the same entry, each incoming edge keeps its own
activation. Withdrawing one edge cannot erase another active edge; the most
recently accepted remaining HIGH supplies the context for local reevaluation.
This does not broadcast an incoming evaluation through unrelated connection pairs.

Changed selected outputs propagate, including LOW. A branch change publishes its
new selected HIGH and withdraws the former output. Sequence admission rejects a
late update on the same connection. Native calls run on an activation edge or a
changed argument tuple; unchanged active arguments reuse the completed result.
A bounded call finishes in the local step before its success/failure edge is followed.
LOW does not itself invoke an inferred action or undo a completed mutation.
Existing authored success/failure and true/false links remain authoritative.

Qualified source notifications remain separate from the outward delta ring. The
bounded queue retains dependency cursors until admission and reports overflow
rather than evicting accepted work. It reads current owned values when evaluation
runs; it is not a historical ledger of samples at notification time. Available
state/time publication and resource release wake blocked evaluation.

### Storage and capacity

Session replies are 20 bytes: epoch `u64`, followed by six `u16` bounds: byte arena,
evaluation slots, native arguments, local components, input bindings and action
records. Defaults are **4096 bytes, 5 work slots, 32 arguments, 8 components,
16 inputs and 16 action records**. Local source admission keeps two work slots
available for incoming/local continuation progress. Metadata is additional fixed
storage; 4096 is the shared value arena, not total executor RAM. The current
release builds allocate 6,736 bytes for the complete executor on either MCU,
excluding resident definitions, the source queue and deferred-work storage.

Retained inputs, output payloads, native argument records and temporary carried/
scratch data share that one arena. Extents use declared lengths, not 512 bytes per
connection. Immutable carried data may be shared; computations writing carried
values receive private working storage. Admission budgets retained capacities plus
all work slots at the largest admitted workspace size. Bounds rejects a binding
that cannot fit; transient work exhaustion returns Busy. Offset-based compaction
preserves aliases and makes released space reusable without a heap. The 512-byte
installation buffer and 8 KiB resident definition arena remain separate owners.

### Signal delivery and readiness

A signal carries the input payload directly. There is no receiver reservation,
separate digest/commit exchange or release receipt. LOW has no payload. HIGH's
length is declared by the target definition; one message carries up to 24 bytes.
Larger updates use fixed 24-byte pieces with an exact final piece and indexed
coverage. Out-of-order pieces and duplicates are accepted without applying partial
values. Incomplete reception expires after 30 seconds; a newer complete update
makes older incomplete updates irrelevant. CRC/framing remain with the transport.

The sender retains each immutable update until the owner accepts it. Requests use
the existing two-second reply deadline and 100–1600 ms capped backoff. Busy leaves
ownership with the sender. A lost reply produces an idempotent retry, not another
native call. One outgoing radio exchange runs at a time; other local evaluations
and deferred device work may overlap. Same-owner continuations use this acceptance path
without RF or a session handshake.

Retained activation alone does not schedule wakeups. Runnable local work, partial
receive expiry, pending sends and retry deadlines contribute actual deadlines;
blocked work waits on source/native/capacity notifications. Radio ISR notification
bits remain separate from network-service wakes. No polling task or periodic
continuity traffic was added. An end-to-end active heartbeat is deferred: an
isolated upstream failure can leave retained HIGH downstream in this interim model.

Statistics retain five `u32` fields: admitted local work, completed local work,
faults, reserved uncertain count (zero), and source overflow; then last fault
status `u8`, definition `u16` and record `u16`. Completion is local, not proof of
whole-circuit or physical completion.

## Maintained checks

Host properties cover declaration bounds, typed initialization, commit publication,
reconstruction, shared-computation ownership, removal, generation reuse and flash
interruption. Execution checks cover initial LOW, head-only startup, local resume
without upstream traffic, independent connection pairs, branch withdrawal, carried
argument changes, stale/duplicate updates, indexed reception, lost requests/replies,
completed-call ownership across LOW, local removal and arena alias preservation.

The retained-input engine has host/property validation and release library builds
for both MCU targets. A C6/nRF52840 bench run installed the same two-owner condition
in both chip directions: source level >= 100, downstream power true, then SetLevel
with source level minus 50. Downstream-only changes invoked locally without another
head admission (14 to 14 with C6 head; 32 to 32 with Nordic head). Changed arguments
while HIGH propagated; unchanged arguments, duplicate and older updates did not
reapply a manually changed target. LOW prevented subsequent local activation.

Both directions completed with zero execution faults and local overflow. Removing
the temporary connections restored the original image extents. Resetting both boards
reconstructed the saved enabled head, started downstream LOW and preserved local
resume after the first HIGH. Surfaces, definitions and clock profiles were preserved;
only the original C6 head's revision advanced for the test's disable/re-enable.
The maintained updated images remain on both boards with test appliance levels zero.

These are one-hop MCU checks using software-backed appliance states, not physical
sensor/actuator, arbitrary RF-delivery or hard latency proof. Native/time primitives
also retain their earlier checks; this run did not repeat temporal-boundary or
isolated-power-loss testing. The lab driver, image hashes and captures are outside
source under `lattice-vertex-lab/experiments/retained-activation-20261105`; no kernel
code or build depends on them.

Both ports' existing radio fixture has a `program-test` feature that starts with
storage and an ordinary test appliance without a signing policy. It contains no
installed automation. Build with `--bin radio --features program-test`; the port's
`update.py prepare --program` packages its loader/application artifacts under
`tests/target/program`. Preparation does not flash a board. Follow the port's
existing layout-3 provisioning procedure with those artifacts before a physical
test. This mode needs no release public key. Signed-update builds use
the same service. `prepare --ota --appliance` provides the existing dimmer fixture
and binary USB host endpoint together; it still contains no preinstalled program.
