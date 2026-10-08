# Arrival-triggered surface publication

Appliance firmware passes its existing static `Surface` to
`kernel::start(context, radio, &SURFACE)`. The kernel borrows the definitions and
publishes them after network selection. Firmware supplies no exposure packet
handler, polling loop, remote registry, or additional networking trait.

Every vertex follows the same path:

1. After joining/forming, broadcast one arrival announcement and publish its own
   surface.
2. On an arrival from the same network, publish its own surface for the newcomer.
3. Accept surface pieces transiently. Ordinary vertices currently discard their
   contents after frame validation; the test observer consumes them synchronously.

Periodic discovery and received surface pieces never trigger publication. An
arrival while a publication is active requests one subsequent complete pass, so
a newcomer does not receive only its tail. Several arrivals before that pass may
share it: these are identical immutable definitions, not distinct state deltas.
Neither nucleus nor a founding vertex has privileged exposure behavior.

Definitions describe the appliance's mathematics. No state reader, native
transformer, or IR evaluator runs while encoding. Current values and their deltas
are a separate, not-yet-connected communication path. Nucleus can retain and
interpret descriptions for synthesis; ordinary vertices do not mirror them.

## Publication envelope

The existing `LVT` version-2 envelope adds two opcodes. Both are broadcast data
frames on the selected channel, with the common PAN filter and no requested MAC
ACK. As with discovery, the network tag is not an authorization credential.

| Opcode | Bytes after the opcode |
| --- | --- |
| 3: arrival | Founding address `u64 LE`, channel `u8` |
| 4: surface part | Founding address `u64 LE`, channel `u8`, publication sequence `u16 LE`, byte offset `u16 LE`, total length `u16 LE`, data |

Surface parts contain 84 bytes except the last, whose size is the exact remaining
length. Offsets must be multiples of 84. A description has at least 13 bytes.
The source MAC address scopes the publication sequence; it increments per local
publication and wraps at 16 bits. This is provisional transfer framing, not a
durable binding incarnation or authenticated replay boundary.

The resident task sends one part at each local 7–13 ms pacing deadline and serves
incoming frames between parts. An API-reported transmit failure abandons that
pass; a later arrival can cause another publication. There is no receipt/retry
protocol yet. Consumers must reject incomplete/gapped transfers. Merely hearing
an arrival, receiving the last part, or seeing a local TX completion does not
establish that a peer received the entire description. Forwarding and a nucleus
consumer have not yet been connected.

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

## Checks

Host tests compare an exact wire vector containing all four surface categories,
shared definitions and direct state references, across several part sizes. They
also check signed literals, expression control, depth/size rejection, canonical
part framing, and finite publication progress. Native readers and transformers
in that test panic if called, establishing that exposure doesn't invoke them.

The port fixture's `peer-observer` feature supplies two small authored appliances:
a switch (one state/transformer) or a dimmer (two states, one transformer, one axiom,
one invariant). The board identity chooses which definition to use. Both enter
the same normal kernel startup path. `SURFACE` output reports completed local
publications and received transfers, a streaming FNV-1a checksum, and received
category counts. The observer retains only the 13-byte header and transfer
progress; it does not retain a received mathematical surface.

For two boards, compare A's `tx_hash` to B's `rx_hash` and vice versa, verify the
opposite category counts, and verify that publication counts stop increasing
while ordinary discovery continues. Start one later, then restart either to
check reciprocal arrival-triggered exposure. Build without observer/probe features
to exercise an explicitly empty surface with no test observer. FNV-1a here is
only a test measurement, not a security checksum or transport authenticity check.

Current validation: ten focused peer/encoding tests pass, as do C6 ordinary,
observer and radio-probe builds and the Nordic observer build. The RF experiment
also compiles through its existing test entry. No C6 USB serial devices were
available for the new live publication check, so no boards were flashed for this
change. Earlier discovery captures are not evidence of surface publication.
