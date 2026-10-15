# Owner discovery and surface reads

Appliance firmware binds its static `Surface` and native device in `kernel::Appliance`,
then passes that binding to `kernel::start(context, radio, &APPLIANCE)`.
The kernel borrows its definitions;
`exposure::Description` emits requested ranges without a second resident IR copy.
No state reader, native transformer, or IR expression executes during a surface read.

Arrivals carry compact owner metadata across relays. Joining, periodic link
discovery, and receiving another arrival never broadcast complete surfaces. A
bounded owner search locates preexisting appliances. Any interested peer can request a
range from a known owner. There is no privileged nucleus read API. Ordinary
vertices serve their own definitions and relay reads/replies, retaining no foreign
IR. The tablet/nucleus consumer, content-based lookup, and surface reconstruction
are not implemented here. [Native control and current-state reads](INVOCATION.md)
use separate messages; surface reads continue to expose immutable mathematics only.

## Progressive discovery direction

The accepted direction is a built-in `ReadLinks` operation over the kernel's
maintained reciprocal peer relationships. An interested peer starts from a nearby
vertex and follows references, reading appliance surfaces along the way. This
requires explicit forwarding through the vertex that supplied a new reference;
a remote address alone does not establish a radio path. The local destination
cache is not the enumerable topology. See the
[kernel direction](../../../docs/KERNEL_DIRECTION.md#maintained-peer-links-and-progressive-discovery).

Maintained links and guided traversal are implemented in the common runtime;
see [the link protocol](LINKS.md). Startup performs local link maintenance;
`FindOwners` remains an explicit bounded search when an interested peer needs it.

## Owner discovery

Each vertex announces its address and encoded description length after joining.
That arrival notice travels up to eight links. Every relay preserves the original
owner and sequence while using its own MAC source address; a recent-notice record
suppresses duplicate copies. Arrival only reports the owner. It never causes a
surface read, another announcement, or another owner search.

An interested peer can send a `FindOwners` query. Each reached vertex remembers
the first incoming neighbor,
returns its own compact `OwnerFound` reply, and forwards the query if scope remains.
Replies from multiple owners follow that same reverse path. Duplicate reply keys
include both query and owner, so the first reply does not suppress other owners.
Matching replies install owner directions in the same cache used by surface reads;
hearing an unsolicited arrival does not confirm a usable reverse direction.

Ordinary periodic channel/network discovery remains link-local. No periodic
network-wide owner scan or full owner catalog is introduced. Its existing interval
also initiates bounded link maintenance. Interested peers can
issue additional owner queries with fresh tokens. Results are incremental: this
protocol has no claim that every owner has replied or that a missing owner is absent.
No description body is sent until an interested peer requests a range.

## Read forwarding

`forwarding.rs` is owned by the resident peer task. Unknown directions cause a
bounded broadcast read; known directions use an addressed next hop. Each relay
records the first incoming neighbor for a request, and the owner returns one
complete response frame along that reverse path. The matching response teaches
relays which neighbor leads toward the owner. The MAC source identifies the
immediate sender; the range record retains the original reader and actual owner.

The shared forwarder holds three learned owner directions, eight recent paths shared
by surface and current-state reads,
four owner-query return paths, eight recent owner notices, four invocation paths,
eight delta duplicate keys, and eight maintained adjacent-peer records, within a
2,048-byte compile-time ceiling. Link enumeration shares the same read paths.
Invocation paths retain exact argument bytes; no foreign definitions are retained.
Read/query/notice records expire after 10 seconds, confirmed directions after 60 seconds without another
matching reply; expiry uses wrapping millisecond arithmetic. Full storage evicts
the oldest record. A failed addressed TX removes that direction. An expired
unanswered read removes its unconfirmed direction unless a newer response has
confirmed it. Subsequent requests then explore again. There is no automatic
retry, route-quality optimizer, guaranteed delivery, or acknowledgement of
executable work in this slice.

A reader must use a fresh 16-bit request token for each attempt, including a retry
of the same range. Tokens must be fresh across surface, current-state, and link reads
within the retained window. Relays
suppress duplicate reader/token pairs without confusing separate range requests;
responses must match the reader, owner, request token, and offset. Full storage or
reboot can shorten duplicate retention, so the eight-hop bound remains necessary.
This identity is for immutable reads, not an exactly-once execution mechanism.

Arrival and owner discovery now cross relays within the same eight-link bound.
Channel formation remains link-local. Adaptive relay selection, content-addressed
sharing, and installed-fragment delivery remain later extensions.
The network tag and matching responses are not authentication credentials.
Startup arrival/query tags use local tick values, not durable boot identities.
A rapid restart or reused token may be suppressed by retained history. Retries
use fresh tokens; history expiry bounds that suppression. This is not an
exactly-once delivery or complete membership protocol. Mutation calls have separate
[request/replay rules](INVOCATION.md#forwarding-and-duplicate-commands).

## Read envelope

`LVT` **version 5** retains version-4 discovery, arrival, owner-query, and range-read
layouts (opcodes 0–7), native invocation/result and deltas (8–10), and current-state
read/result (11–12). Link maintenance and enumeration add opcodes 13–15 as specified
in [LINKS.md](LINKS.md#wire-format). These extensions preserve existing v5 layouts;
older version-5 images ignore unsupported opcodes. Earlier versions do not interoperate. MAC framing still uses numeric 2006 data frames,
compressed PAN `0x4c56`, extended source, and broadcast or extended destination.
No MAC ACK is requested. Integers are little-endian.

Read and range-reply messages begin after their opcode with the network founder (`u64`) and
channel (`u8`), followed by a `SurfaceRead`: reader (`u64`), owner (`u64`), request
(`u16`), and byte offset (`u16`). These owner/reader addresses are not graph IDs.

| Kind | Remaining fields | Delivery |
| --- | --- | --- |
| 4: read surface | Remaining hop budget (`u8`, 1–8) | Addressed along a learned direction, otherwise broadcast |
| 5: surface part | Total encoded length (`u16`), distance from owner (`u8`, 1–8), bytes | Addressed along the recorded reverse path |

A reader starts with hop budget 8 (or less); a relay decrements it. A node at
budget 1 can answer for itself but cannot forward farther. The owner starts the
reply distance at 1 and each relay increments it. A reply's largest addressed
frame has a 21-byte MAC header, a 37-byte Lattice envelope, and **67 bytes of data**,
fitting the 125-byte MAC limit before hardware FCS.

Offsets need not be aligned. Each response contains exactly
`min(67, total - offset)` bytes. Offset equal to total returns an empty terminal
range; an owner ignores offsets beyond its description. The request fields,
length, and frame checksum identify a complete individual range. Consumers still
need all required ranges to reconstruct a complete definition and must handle
missing replies. No whole-surface delivery is inferred from the final range.

## Discovery envelope

Arrival/query/reply payloads start with the same four-byte magic, opcode, founder
(`u64`), and channel (`u8`). Remaining fields are:

| Kind | Fields after network | MAC delivery | Total bytes before FCS |
| --- | --- | --- | --- |
| 3: arrival | Owner (`u64`), sequence (`u16`), description length (`u16`), distance (`u8`) | Broadcast | 42 |
| 6: find owners | Seeker (`u64`), request (`u16`), remaining scope (`u8`) | Broadcast | 40 |
| 7: owner found | Seeker (`u64`), request (`u16`), owner (`u64`), description length (`u16`), distance (`u8`) | Directed reverse path | 56 |

The owner description length is at least 13 bytes. Arrival and owner replies start
at distance 1 and relays increment it; an eight-link notice may be consumed but
cannot travel farther. Queries start with scope 8 or less; scope 1 permits the
local owner's reply but no additional forwarding. All messages fit one radio
frame. Discovery return paths remain open for multiple owner replies until expiry
or eviction. The node stores only recent duplicate keys and return directions,
not the advertised descriptions or a record for every vertex in the network.

## Mathematical description encoding

`exposure.rs` encodes the pinned `lattice-ir` vocabulary. Numbers are little-endian;
no native Rust struct layout is copied. A reference is the original declaration's
address zero-extended to `u64`, scoped by the sending peer and firmware image.
These are opaque remote references, never pointers to dereference locally and
never proof of permission to invoke an operation. Native reader/invocation function
pointers and current appliance values are not serialized.

The header is `4c 53 46 01` (`LSF`, version 1), sender pointer width in bits (`u8`),
then four `u16` counts: states, transformers, axioms, invariants. Records follow
in that order, retaining the surface's declaration order:

| Record | Encoding |
| --- | --- |
| State/attribute reference | Declaration reference, scalar type |
| Transformer | Native handle reference, precondition predicate, effect |
| Axiom/invariant | Predicate declaration reference, predicate |
| Predicate | Definition, list of bound input attributes |
| Definition | Definition reference, argument-type list, symbolic-input-type list, body expression |
| Effect | Argument-type list, input-attribute list, output-attribute list, result-expression list |

Every list begins with its `u16` count. Attribute references include their types,
including those reached through predicates/effects. Shared definition references
retain their declaration address and include the referenced body inline. There
is no assigned-ID table, copied local state, or allocated serialization graph.

Scalar types: 0 Boolean; 1–4 unsigned 8/16/32/64; 5–8 signed 8/16/32/64. A literal
contains its scalar type then its value: Boolean 0/1 in one byte, or the integer's
declared-width bit pattern. Negative integers retain two's-complement bits.

Expressions use prefix encoding:

| Tag | Following fields |
| --- | --- |
| 0 constant | Typed literal |
| 1 read | Attribute reference |
| 2 argument / 3 symbolic input | Position `u16` |
| 4 not | Child expression |
| 5 and / 6 or | Left, right expressions, retaining lazy evaluation meaning |
| 7 unary | Unary code, optional target integer type, child |
| 8 binary | Binary code, left, right |
| 9 select | Condition, true alternative, false alternative |
| 10 reference | Referenced definition |

Unary codes are 0 not, 1 complement, 2 zero-extend, 3 sign-extend, 4 truncate,
5 reinterpret; conversions include the target integer-type code. Binary codes
0–16 are equal, not-equal, less, less-equal, greater, greater-equal, add, subtract,
multiply, divide, remainder, bit-and, bit-or, bit-xor, shift-left, shift-right,
arithmetic-shift. Semantics remain those of `lattice-ir`; this encoding introduces
no execution semantics of its own.

The kernel counts/checks the description before starting the scheduler. The
current bounds are 65,535 encoded bytes and 32 expression levels; too-large or
too-deep definitions fail startup rather than emitting a misleading partial
surface. This is a transport/encoder bound, not a new IR language rule. Each part
is generated by walking the immutable definitions up to that byte range. This
trades repeated traversal for fixed RAM and avoids retaining an encoded copy.

## Checks and current status

Peer tests exercise arrival and owner-query propagation across two relays,
multiple independent owner replies, cyclic duplicate suppression, discovery
followed by an addressed surface read, and expired/evicted discovery paths.
Range-read checks cover cold exploration followed
by directed reuse, duplicate paths and cycles, independent tokens, response
correlation, hop limits, invalid owner offsets, bounded eviction, TX failure,
expiry, and clock wrap. Existing encoder vectors still check the IR vocabulary
and assert that native readers/transformers never execute. Random/repeated range
reads are independent of any publication cursor.

The passive board observer supplies a switch or dimmer surface and logs
`SURFACE_PART` ranges with reader, owner, token, offset, total, RSSI, and per-range
checksum. Its counters count requests and parts, not completed surfaces. It
originates no reads, and pairing two uninterested vertices intentionally produces
no surface traffic. A consumer is needed to exercise reads on hardware.

RF/range investigation remains paused. This revision is validated by host tests
and both MCU builds, not a new hardware run. Older broadcast-capture results do
not validate the current version-5 behavior; see [validation scope](TESTING.md#validation-scope).
