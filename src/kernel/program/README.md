# Resident programs

This component receives previously unknown local program definitions, validates
them, commits them to protected storage, and attaches their source relationships
to the appliance's owned states. Startup rebuilds those links from committed
definitions against the current native bindings. Firmware supplies no automation
callback, installation loop, storage policy, or message handler.

**Installed means committed, validated and wired, but inactive.** A separate durable
head-admission command enables execution. The kernel follows upstream activation
links, evaluates the installed local records, transfers complete visits and invokes
native functions in existing RTOS tasks. Firmware supplies no evaluator callback.
Local time/solar records use kernel deadlines and the same upstream activation
path. Externally installed retained-state maintenance remains unsupported.

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
admitted. Its epoch cannot alias tickets issued before restart. This is not a
mutation journal.
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
| 7 temporal | Activation entry `u16`, temporal predicate, true/false links |

A temporal predicate is tag 0 plus start/end `u32` local seconds, or tag 1 plus
two solar boundaries. Each solar boundary is event `u8` (sunrise=0, noon=1,
sunset=2) and signed offset seconds `i32`. The record registers a local source even
when a preceding condition short-circuits. Its activation entry must precede and
reach it, and lead upstream to a head/foreign owner. It consumes no carried slot.
The [time component](../time/README.md) owns its clock/deadline semantics.

The invoke record describes continuation on the returned native result. It does
not infer physical completion or encode an acceptance-only continuation. A
physical completion dependency belongs to an observed state. The existing native task returns its result before that visit follows either edge;
other visits, acquisition and radio service can continue meanwhile.

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
does not allocate traversal contexts. Enablement and incoming visits check execution
capacity separately. Firmware callbacks cannot substitute for missing record kinds.

## Persistent image

The committed payload starts with `LPG`, version byte 3, followed by local
definitions and an optional commissioned clock profile. Each has a 24-byte prefix: total prefix-plus-definition length
(`u16`), enabled flag (`u8`, 0 or 1), record kind zero (`u8`), generation (`u32`),
installing caller (`u64`), transfer nonce (`u32`), and admission revision (`u32`).
LPG v1 is read as disabled with admission revision zero. V2 admission is preserved.
The startup checkpoint upgrades either header and preserves every definition offset and generation. Runtime
source links and the pending-initial-evaluation flag are excluded from saved/read bytes. Definitions append to this
image; there is no deletion, live replacement or reuse of occupied extents yet.
Generations do not wrap. The immutable caller/nonce and canonical definition
digest distinguish a repeated commit from conflicting work after a lost reply or
restart. A reservation receipt is not a commit receipt.

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

Clock commands 15–18 share this service; layouts and revision semantics are in the
[time component](../time/README.md#commissioning-and-reacquisition).

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
Execution adds Enabled (12), Disabled (13), Accepted (14), Stale (15), and Unavailable
(16); an Accepted visit receipt does not claim that its native operation has finished.
Data ranges are ordered; an identical duplicate is acknowledged without another
copy. Conflicting bytes/digests fail. Persisting/installed receipts are idempotent
for the same caller/nonce/definition. An abandoned incomplete reservation cannot
occupy installation capacity indefinitely. This is structural admission and
transport integrity, not a new authentication or FSPS proof mechanism.

## Admission and execution

The same LVT program route and host envelope carry execution control. Read tokens
are allocated jointly by the host endpoint and resident executor. Routing remains
owner-directed through the existing peer paths; neither tablet nor compiler owns
runtime forwarding.

| Command | Fields after command byte |
| --- | --- |
| 7 admission state | Definition offset `u16`, definition generation `u32` |
| 8 set admission | Same reference, expected admission revision `u32`, enabled `u8` |
| 9 receiver session | None |
| 10 reserve visit | Receiver epoch `u64`, sender epoch `u64`, sender sequence `u32`, entry reference (definition `u16`, generation `u32`, record `u16`), upstream `u8`, carried length `u16`, cause (owner `u64`, epoch `u64`, sequence `u32`) |
| 11 visit data | Receiver ticket (epoch `u64`, slot `u16`, generation `u32`), offset `u16`, byte count `u8`, 1–38 bytes |
| 12 commit visit | Receiver ticket, SHA256 of the carried bytes |
| 13 release receipt | Receiver ticket |
| 14 execution statistics | None |

Admission replies identify the definition and include its current revision as four
bytes. Enabling/disabling is a compare-and-set against that revision. Persistence
completes before admission changes; repeating the accepted request cannot start a
second enablement evaluation. Initial enablement schedules one fresh activation.
Disablement stops admission at the head and does not cancel previously admitted
forward visits. Distributed fragment/interface readiness remains the authoring
peer's responsibility; a local enable receipt does not prove remote readiness.

Session replies contain the durable epoch (`u64`), working-byte capacity (`u16`),
visit count (`u16`) and native argument capacity (`u16`). Current kernel build
bounds are four independent visits, one 4096-byte working arena, 32 pending local
causes and 32 typed native arguments per call. Each visit reserves only its
declared carried-plus-scratch extent in that arena; no heap is used. The manual
call codec's 61-byte limit does not constrain program arguments or carried data.
A local Entry starts with empty scratch and its declared carried inputs. Only
current, established native observations are read; unavailable values are neither
false nor zero. A visit waiting for an unavailable input retains its progress.

Native observation/history publication records each cause separately from the
outward eight-entry delta ring. After the batch commits, each pending source cursor
advances only when its dependent activation is admitted. Full local cause storage
increments the explicit overflow counter without evicting older work. An upstream
visit follows Entry links without testing predicates; the enabled head starts the
forward path. True/false successors encode short-circuit control. Local arithmetic
uses the shared IR's declared-width semantics. Independent visits own independent
values, including while waiting for a native function's return.

Delivery obligations follow these installed dependents, not every vertex in the
network. Relays route to the named owner without interpreting the program. An
observed traversal is not a substitute for the owner's correlated acceptance.
An upstream activation is the addressed notification of a source delta. Its first
remote delivery is necessary for the receiving vertex to continue traversal;
local registration is not a substitute for getting that notification across the radio.
The `dependent_delivery` boundary table drops one request or reply at each handoff
phase of a forward transfer and checks invocation count, retained progress and
independence from `StateChanged` broadcast eviction. It does not separately inject
loss into a remote upstream activation. These host checks do not establish crash
recovery or arbitrary multi-hop radio delivery.

A foreign handoff first reserves a receiver context and working extent. Ordered
chunks are acknowledged without evaluation; identical duplicates are harmless and
conflicting bytes are rejected. Commit checks completeness, digest and typed Entry
inputs before admitting the visit. The acceptance receipt is retained separately
from the running visit. Repeating Commit returns the same acceptance without
re-evaluation. Sender ownership ends on acceptance; Release only frees the receipt.
Losing Release does not cancel the receiver's work.

Receiver tickets include boot incarnation and slot generation. An expired/reused
or pre-reboot ticket cannot invoke again. A sender may restart an expired partial
transfer before Commit, but an uncertain Commit is never restarted under a new
ticket. Stale evidence at that point becomes an explicit uncertain outcome. Partial
reservations expire after 30 seconds without accepted data; acceptance receipts
expire after 60 seconds or Release. Neither expiry discards an admitted visit.
An ordinary request waits two seconds for a response and uses capped exponential
retry delays of 100–1600 ms. Busy retains the sender's work. Persistent unavailability
can occupy fixed visit capacity; it does not allocate more memory or silently drop
that visit. One outgoing exchange is in flight per radio service; local/native
visits still overlap.

Statistics replies contain five `u32` counters (admitted local visits, completed
local visits, faults, uncertain handoffs, local-cause overflow), followed by the
last fault status (`u8`), definition (`u16`) and record (`u16`). Local completion
includes stopped paths and accepted onward handoffs; it is not whole-program or
physical completion. These diagnostics remain available through the ordinary peer
service. Reboot recovery walks restored source bindings upstream and can cause
repeated evaluations across returning participants; coordinated reconciliation is
explicitly deferred.

## Maintained checks

Host checks enforce declaration bounds, typing, initialization, publication after
commit, duplicate/conflict behavior, reservation expiry and reconstruction against
fresh native storage. Execution checks cover declared-width arithmetic, location
bounds, commit-before-admission, independent activations despite transport eviction,
conditional cross-owner execution, partial handoffs, duplicate commits and capacity.
Shared transport checks cover the command and message boundaries.
Both ports' existing radio fixture has a `program-test` feature that starts with
storage and an ordinary test appliance without a signing policy. It contains no
installed automation. Build with `--bin radio --features program-test`; the port's
`update.py prepare --program` packages its loader/application artifacts under
`tests/target/program`. Preparation does not flash a board. Follow the port's
existing layout-3 provisioning procedure with those artifacts before a physical
test. This mode needs no release public key. Signed-update builds use
the same service. `prepare --ota --appliance` provides the existing dimmer fixture
and binary USB host endpoint together; it still contains no preinstalled program.

The 2026-10-22 C6/nRF52840 hardware check installed 133-byte and 135-byte definitions
after boot, including radio installation of the remote half. It evaluated
`C6.level >= 100 && Nordic.power`, computed `C6.level - 50` at the C6, and invoked
Nordic SetLevel with that carried byte. False predicates suppressed mutation;
changing only Nordic power activated backward through the C6 and returned to the
Nordic. Enable/disable/resume, registered delta events, and reconstruction after
resetting both MCUs passed, with no reported execution faults, uncertain handoffs
or activation overflow in the capture. These are software-backed native surfaces;
the result does not establish physical dimmer behavior or loss-free RF. Artifacts,
captures and the one-off authoring/check script remain in the external lab.
