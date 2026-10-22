# Native invocation and observed state

In the basic vertices-and-tablet tier, an ordinary tablet daemon peer addresses an
appliance's exposed native transformer; the kernel invokes it and communicates
subsequent established state changes. No nucleus, solver, IR interpreter, learned
surface, or compiled-fragment runtime is required. Any peer can also read one
exposed attribute to establish its initial value. The optional [attached host endpoint](host/README.md) and the shared Linux client in
`lattice-tools` (also consumed by `lattice-tablet`) originate these calls and reads. Registered local changes and
accepted remote deltas also stream to an attached host without state polling.
The host event buffer is bounded and reports oldest-event eviction separately
from command results; it does not guarantee RF delivery or retain state history.

This is native control, not distributed program installation. The callback-based
fragment prototype and its transport buffer have been removed. Resident records
now load and wire through the [program component](../program/README.md); upstream
activation, forward execution and per-traversal storage remain
[execution work](../../../docs/KERNEL_DIRECTION.md#next-implementation-milestone).
Experimental LVT v7 opcodes 19/20 and native invocation result statuses 6/7 are retired and rejected;
do not reuse them for different semantics within this wire version. Native
invocation opcodes 8/9, statuses 0–5, and host-v4 encoding are unchanged.

## Appliance ownership

Firmware declares `State`, `StateTransformer` and a static `Surface`, initializes
one `kernel::Appliance<Device, Error>`, then enters `kernel::start`. It supplies
hardware getter/setter hooks; it has no monitoring/reporting loop or delta API.
The adapter is `Send + Sync`; getters and transformers take shared `&Device` access.
Hardware bindings protect their own short peripheral critical sections rather
than exclusively borrowing the whole appliance throughout a long operation.

The kernel starts one acquisition task and two native-call tasks, each currently
reserving 4 KiB of static stack and an RTOS control block. These kernel build bounds
live in `config.rs`, alongside the 32-native-state observation capacity and 10 ms
fallback sampling interval. The interval follows completion of an acquisition pass;
it is not a guaranteed sample deadline. Direct hardware-event acquisition is future
port integration, and sampling cannot recover unobserved intervening transitions.

The acquisition task calls the supplied getters and owns the previous/current
established observations. Native calls also request acquisition before and after
execution, including on error. The first pass establishes a baseline without
inventing a delta or history sample. Subsequently unchanged values emit nothing;
availability changes are real changes. Physical I/O holds no observation-cache mutex,
so reads can return the preceding established observation while acquisition runs.

A complete pass captures native inputs, records changed states, maintains history,
then releases publication to transport. Source and derived deltas retain distinct
sequence numbers and normal hard eviction. No report callback belongs in firmware.
The old `StateHandle::update`, `changed`, and local `Dependent` API/example were
removed; retained native history helpers remain separate from program installation.

Invocation reserves a native task and owns its argument bytes. A matching pending
copy joins the same invocation and up to two return directions. Full execution
storage reports Busy; there is no waiting-job queue. The worker decodes arguments,
resolves the native handle, checks its signature, and invokes the function once.
Completion wakes the peer task to retain/return its status over RF or local USB.
Starting a task does not produce an `Invoked` result. Long finite functions do not
block peer dispatch or exclude getter access through a whole-appliance mutex.

Pending invocation records cannot be evicted by completed-history churn. A host
session ending or timing out does not cancel admitted hardware work. Existing
8-second host deadlines and 10-second relay paths still bound response delivery;
a late result is not guaranteed after its path expires. No timeout starts another
mutation. Hardware success and established state remain separate facts.

With updates enabled, admission holds flash staging/activation until the call
finishes. Acquisition uses a quiet hold: it excludes conflicting flash/commit work
without resetting the update quiet interval merely because a getter ran. Actual
registered changes still count as activity.

## One frame per call, read, result, or delta

These use **LVT version 7** and the MAC/network profile in
[EXPOSURE.md](EXPOSURE.md#read-envelope). Versions before 7 are incompatible.
All numbers are little-endian; no strings or Rust layouts travel on the wire.
The common payload prefix is magic/version (4), opcode (1), network founder (8),
and channel (1).

| Opcode | Fields after the common prefix | MAC bytes including payload, before FCS |
| --- | --- | --- |
| 8: invoke | Caller `u64`, owner `u64`, request `u32`, transformer reference `u64`, packed scope/copy budget `u8`, argument count `u8`, typed scalar arguments | Addressed: 65–125; one Boolean argument: 67 |
| 9: invocation result | Same caller/owner/request/transformer, status `u8`, distance `u8`, path cost `u32` | Addressed: 69 |
| 10: state changed | Owner `u64`, attribute reference `u64`, sequence `u32`, distance `u8`, typed scalar value or unavailable tag | Broadcast: 51–59; Boolean: 52 |
| 11: read state | Reader `u64`, owner `u64`, request `u16`, attribute reference `u64`, remaining scope `u8` | Addressed: 62; broadcast exploration: 56 |
| 12: state value | Same reader/owner/request/attribute, distance `u8`, result tag `u8`; successful read adds change sequence `u32` and typed scalar or unavailable tag; ends with path cost `u32` | Addressed: 67 for a failure; 72–80 for a value; Boolean: 73 |

Replies (opcodes 9 and 12) append a Q24.8 microseconds-per-byte path cost;
`0xffffffff` is unknown and an owner starts at zero. See the cost model in
[the shared wire definitions](wire/README.md#communication-cost).

The invocation control byte keeps remaining hops (1–8) in bits 0–3. Bit 4 is
`copies - 1`: zero permits one copy, one permits a single split into two copies.
Bits 5–7 are reserved and must be zero. An addressed origin can request two-copy
delivery; a broadcast invocation must carry a one-copy budget. This is protocol
metadata, not an appliance setting, and it adds no frame overhead.

Scalar tags and declared-width values are identical to surface literals: 0 is
Boolean (only 0/1 accepted), 1–4 are unsigned 8/16/32/64-bit, 5–8 are signed
8/16/32/64-bit. Tag 255 means unavailable and is valid only for a state value.
The entire argument section has a 61-byte limit, including its count: at most
30 one-byte scalars or six 64-bit scalars. Oversized calls cannot be fragmented
by this protocol. Truncation, trailing bytes, invalid tags, and invalid scope
are rejected before native dispatch.

Statuses are 0 `Invoked`, 1 `Arguments`, 2 `UnknownTransformer`, 3 `ApplianceError`,
4 `Busy`, 5 `ConflictingRequest`. `Invoked` means the native function returned
success. It does not mean the physical appliance reached the modeled post-state.
An error can follow a real partial change; that observation still emits its delta.

## Forwarding and duplicate commands

Invocations reuse owner directions learned from correlated discovery, read, or
invocation replies. Each of three cached owners can retain up to two different next
hops. Selection favors lower composed time per acknowledged payload byte, then
hop count/address for ties. A direct neighbor competes on the same cost; a cheaper
measured indirect path can win. Unknown costs rank after known costs. Correlated
replies carry the sender-to-owner suffix; the receiving node adds its outgoing
link cost. Round-trip latency no longer sets route preference.
Alternates must first be learned from actual correlated exchanges or guided
traversal; this increment does not add a background route-probing workload.

With two eligible directions and a two-copy budget, a relay emits a pair of
addressed copies immediately and gives each a one-copy budget. A single known
direction carries an unused budget onward. Unknown directions use the existing
bounded broadcast exploration with a one-copy budget. That flood can reach more
than two receivers, but none may split it into another addressed pair. Delta and
arrival broadcasts retain their existing behavior. No execution timeout triggers
another native operation.

The four call records retain exact arguments, up to two incoming directions,
up to two outgoing directions, and the returned status. A pending matching copy
from another upstream direction joins the return paths without forwarding the
work again. Only equal/shorter incoming branches (remaining hops at least the
first copy's) are admitted; a copy arriving back from downstream must not create
a return-path cycle. Thus reconvergence through a later, longer incoming branch
is not retained as an additional return path in this increment.

A matching result can return through both retained incoming directions. Up to two
distinct returning neighbors can establish owner directions; repeats from the
same neighbor or conflicting statuses are discarded. A late matching request
receives the saved status along its own retained incoming direction. Cached
results never contain or substitute for physical state.

Paths expire after 10 seconds or oldest eviction. Failed addressed TX removes
only that candidate. When a call expires, each unanswered outgoing direction is
withdrawn unless newer evidence confirmed it; success through the other branch
does not preserve a silent direction. Candidates expire independently after 60
seconds without confirmation. No route record or radio completion proves execution.

At the owner, eight recent invocation records retain caller/request, transformer reference,
exact arguments, and returned status. An identical duplicate returns that status.
Reusing caller/request for different content returns `ConflictingRequest`.
These records are replaced in insertion order, and do not survive reboot.

The MAC transport can retransmit an identical addressed frame up to three times
when its ACK is missing or CCA is busy. The receiver ACKs a duplicate but suppresses
its delivery while recent MAC history is resident. All copies retain the original
call token and arguments; the owner call-history rules still apply after routing.
An ACK proves one-hop frame reception, not native execution or physical completion.
Exhausting MAC attempts after a transmission is uncertain delivery. The original
call remains pending for its result/normal timeout, including a result received
while waiting for the missing ACK. It is not immediately failed as unsent.

There are **no automatic command retries with a new invocation identity**. A caller must not use a fresh token to
retry an uncertain mutation as though it were an immutable surface read. Result
replay is bounded by retained history; after eviction or reboot a repeated request
can execute again. This is not an exactly-once guarantee or durable execution
ledger. Firmware references are scoped to the owner's current image. Authentication
and reboot/session identity remain subsequent protocol work.

## Observation propagation

Kernel acquisition compares captured getter values with the last established
local observations. Only declared native states and installed history views of
the bound instance enter `kernel::change`. A batch becomes visible to transport
after native cache/history maintenance completes; notification coalescing does
not merge the individual records.

`kernel::change` retains eight pending attribute/sequence/value records with hard
oldest eviction and a counter. It is neither a foreign-state mirror nor a graph
execution queue. A short interrupt-masked section protects metadata on the
supported single-core ports; no hardware I/O runs there. The radio task wakes for
published records, while its existing receive/TX completion rules remain intact.

Each accepted delta can cross eight links. Relays retain eight recent owner/sequence
keys for 10 seconds, suppressing transport duplicates without merging distinct
changes. Remote deltas never overwrite the local hardware adapter. Resident upstream
activation and forward program traversal remain unimplemented. Sequence numbers
restart on reboot; loss, eviction and restart do not imply automatic recovery.

### Retained native state

`kernel::state::History` and `RetainedState` use native-width static storage.
`Appliance::install_state` binds one source history and a firmware-compiled updater
before startup. Acquired changes append captured post-values and evict oldest
when full. Missing values remain unavailable; unchanged acquisitions add no sample.
Acquisition finishes this maintenance before publishing the batch.

Each history position and result is an exposed scalar with a mathematical
definition. Normal reads use the observation-cache lock and changes use the
ordinary delta ring. One acquired change can shift several views, consuming one ring entry per changed
view and incurring the same hard eviction. History storage itself is not copied
onto the stack or transmitted as a packed history record.

The native history updater accepts one source history and no mutable accumulator
input. Publication seals these startup definitions; OTA surface fingerprints include them. These are
current native helper limits, not the compiler-program interface. External maintenance
operations, multiple sources and post-startup installation remain design work;
immutable surface publication must not prohibit later program installation.

## Reading current state

A peer learns an attribute reference/type from its owner's surface and sends
`ReadState` when it needs the current established observation. Each read and result
is complete in one frame. The request does not add network polling, an initial
broadcast or an all-appliance snapshot. The reply returns along the same bounded read
paths used by surface requests; it is never a state-change broadcast.

At the owner, `Appliance` matches the reference against its declarations and
returns the latest kernel-acquired observation with the owner change watermark
under the observation-cache lock. It does not run physical I/O on the peer task,
invoke a transformer, maintain history, increment a sequence or consume outgoing
deltas. Acquisition continues independently. Contention with the brief publication
step returns Busy; an unacquired or unavailable state returns an unavailable value.

Result tag 0 carries a value/sequence; the typed value can be unavailable (255).
Tag 1 means `UnknownAttribute`, and tag 2 means `Busy`; neither carries a sequence
or value. Unavailable is an actual observation state and is distinct from a read
failure, false, or zero. A retry uses a fresh read token and obtains the latest acquired value; relays
and owners do not replay an earlier request's result. Duplicate copies of the same read
are suppressed within the existing retained-path window.

Surface, state, and link reads share eight fixed path records and one reader/token
namespace. A request token must be fresh across all read kinds while history
is retained. Replies must match the original kind, owner, token, and requested
offset or attribute. Unknown directions explore within eight links; matching
replies confirm directions, including `Busy` and `UnknownAttribute`. The existing
expiry, oldest eviction, and addressed-TX-failure behavior applies to both kinds.
No additional routing table or foreign-state cache is introduced.

The sequence is the owner's watermark when the value was sampled, not a per-state
revision. A consumer can retain it for that attribute and use wrapping `u32`
sequence comparisons to distinguish delayed replies from newer deltas, within
one boot and less than half the sequence space. It must not apply an attribute's
read watermark to unrelated attributes. Reads of different attributes are separate
samples and do not promise a mutually consistent multi-attribute snapshot.
Boot/session identity remains future work; this watermark does not establish
ordering across reboot, compensate for packet loss, or prove physical convergence.

## Storage and validation

The common forwarder retains its 2,464-byte compile-time ceiling, including three
owners with two candidate directions each, maintained adjacent links, read/discovery
records, four call paths with two-way branch bookkeeping, and eight delta duplicate keys. Owner
invocation history has a 1,280-byte compile-time ceiling, including active-call
tickets and return directions. Appliance and transport storage are
static; argument decoding uses bounded stack storage. No allocator is enabled.

Component checks are owned by the code whose properties they constrain:

| Owner | Boundaries checked |
| --- | --- |
| `peer::invocation` | Exact replay key/payload, admission, pending identity protection, result replay and completed-history eviction |
| `appliance` | Native signature/reference admission, acquired-value transitions, reads without delta consumption and partial hardware effects |
| `change` | Instance/reference filtering, FIFO capacity, sequence wrap and hard eviction |
| `peer::forwarding` | Read correlation, hop and copy budgets, incoming direction limits, normalized route ranking, expiry and bounded duplicate history |
| `peer::host` | Session correlation, timeout without retry, result preservation, reply/event separation and bounded event loss |
| `frame` and `wire` | Numeric encoding and frame-size/field bounds |

Tables partition these local boundaries without simulating several peers or
reimplementing appliance behavior. Replay checks apply while the result remains
retained; they do not extend that guarantee across history eviction or reboot.
Host checks and C6/nRF ordinary and OTA builds cover this revision; its new
RTOS acquisition/call task integration has not yet been exercised on hardware. Earlier callback-chain hardware captures remain in
the external lab as evidence of native calls, retained-state maintenance and
operand transport. They do not validate an externally installable distributed
program. RF investigation remains paused.
