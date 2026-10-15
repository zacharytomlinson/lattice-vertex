# Native invocation and observed state

This is the basic installation path: an ordinary tablet daemon peer addresses an
appliance's exposed native transformer; the kernel invokes it and communicates
subsequent established state changes. No nucleus, solver, IR interpreter, learned
surface, or compiled-fragment runtime is required. Any peer can also read one
exposed attribute to establish its initial value. Tablet transport/consumer code
is not implemented in this repository yet.

## Appliance ownership

Firmware declares its existing `State`, `StateTransformer`, and static `Surface`,
then reserves one `kernel::Appliance<Device, Error>` with `Appliance::new(&SURFACE)`.
Before scheduler startup it calls `APPLIANCE.initialize(&context, device)`, and
passes `&APPLIANCE` to `kernel::start(context, radio, &APPLIANCE)`.

The binding owns the actual native instance, one static RTOS mutex/control block,
and the borrowed surface. It does not copy the appliance's values into a second
state table. Hardware reporting tasks use `APPLIANCE.with(&context, |device| ...)`
and the existing `StateHandle::update` or grouped `observation::update` inside
that closure. These are task-context operations. The kernel provides the networking;
firmware still establishes which physical observations actually occurred.
The closure must not reacquire the same appliance. Native transformers should
return promptly after performing or registering device I/O; physical completion
may arrive later through a reporting task.

Remote calls use the same mutex without waiting behind hardware work: contention
returns `Busy`. Native dispatch matches the supplied reference against the exposed
transformer handles, decodes its typed arguments, and invokes the matching adapter.
A numeric remote reference is never dereferenced as a pointer. Native implementations
remain responsible for hardware limits; the kernel does not interpret IR preconditions.

## One frame per call, read, result, or delta

These use **LVT version 5** and the MAC/network profile in
[EXPOSURE.md](EXPOSURE.md#read-envelope). State reads add opcodes 11–12 without
changing existing messages. Earlier version-5 firmware ignores those new opcodes;
versions before 5 remain incompatible. All numbers are little-endian; no strings or Rust layouts travel
on the wire. The common payload prefix is magic/version (4), opcode (1), network
founder (8), and channel (1).

| Opcode | Fields after the common prefix | MAC bytes including payload, before FCS |
| --- | --- | --- |
| 8: invoke | Caller `u64`, owner `u64`, request `u32`, transformer reference `u64`, remaining scope `u8`, argument count `u8`, typed scalar arguments | Addressed: 65–125; one Boolean argument: 67 |
| 9: invocation result | Same caller/owner/request/transformer, status `u8`, distance `u8` | Addressed: 65 |
| 10: state changed | Owner `u64`, attribute reference `u64`, sequence `u32`, distance `u8`, typed scalar value or unavailable tag | Broadcast: 51–59; Boolean: 52 |
| 11: read state | Reader `u64`, owner `u64`, request `u16`, attribute reference `u64`, remaining scope `u8` | Addressed: 62; broadcast exploration: 56 |
| 12: state value | Same reader/owner/request/attribute, distance `u8`, result tag `u8`; successful read adds change sequence `u32` and typed scalar or unavailable tag | Addressed: 63 for a failure; 68–76 for a value; Boolean: 69 |

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

Invocations reuse owner directions learned by discovery, surface replies, or
invocation results. Without a direction they explore by bounded broadcast. Each
relay retains the first incoming neighbor; the owner returns its result along
that path. Scope is at most eight links. The owner invokes only its own exposed
handle. Relays do not execute the call or evaluate mathematical definitions.

Four fixed call-path records retain the full request, exact argument bytes,
incoming/onward directions, and optional returned status. A pending duplicate is
suppressed. A matching duplicate after the result arrived can recover the saved
result without another native invocation. Paths expire after 10 seconds or are
evicted oldest-first. Failed addressed TX or an expired unanswered call invalidates
its unconfirmed direction, using the same rule as surface reads.

At the owner, eight recent invocation records retain caller/request, transformer,
exact arguments, and returned status. An identical duplicate returns that status.
Reusing caller/request for different content returns `ConflictingRequest`.
These records are replaced in insertion order, and do not survive reboot.

There are **no automatic command retries**. A caller must not use a fresh token to
retry an uncertain mutation as though it were an immutable surface read. Result
replay is bounded by retained history; after eviction or reboot a repeated request
can execute again. This is not an exactly-once guarantee or durable execution
ledger. Firmware references are scoped to the owner's current image. Authentication
and reboot/session identity remain subsequent protocol work.

## Observation propagation

`StateHandle::update` and `observation::update` compare established before/after
values. An unchanged value emits nothing. `StateHandle::changed` registers one
explicit delta even when a previous value is unavailable. Only exposed handles
on the exact bound native instance are published.

For a grouped hardware report, the kernel captures all changed values and registers
them before local dependent callbacks can create newer observations. Local callbacks
continue to execute synchronously and read current native state. Distinct registered
changes retain distinct sequence numbers, including changes to unavailable state.

`kernel::change` owns an eight-entry transport ring. It retains only pending
attribute/sequence/value records, with hard oldest eviction and an eviction counter.
This is neither an appliance-state mirror nor a graph execution queue. A short
interrupt-masked section protects its metadata on the supported single-core ports;
no native I/O occurs there. Notification wakes the existing radio task. Once formation
finishes, pending deltas interrupt its receive wait, while TX/energy operations retain
their ordinary completion/cancellation rules. Coalesced wakeups do not coalesce records.

Each accepted delta can cross eight links. Relays retain eight recent owner/sequence
keys for 10 seconds, suppressing duplicate transport copies without merging distinct
changes. A received remote delta never overwrites local appliance state. Installed
conditional activation remains later work. State sequence numbers restart on reboot;
rapid restart can collide with retained history. Missing frames, ring eviction, or
history eviction are not recovered by a retry engine or a current-state cache.

## Reading current state

A peer learns an attribute reference/type from its owner's surface and sends
`ReadState` when it needs the current established observation. Each read and result
is complete in one frame. No periodic polling, automatic initial broadcast, or
all-appliance snapshot is added. The reply returns along the same bounded read
paths used by surface requests; it is never a state-change broadcast.

At the owner, `Appliance` matches the reference against its exposed state handles
and acquires its existing mutex without waiting. It calls that state's reader
once and pairs the value with the latest registered local change sequence while
still holding the mutex. This neither invokes a transformer, runs dependents,
increments the sequence, nor drains the outgoing delta ring. A previously sent or
evicted delta does not erase the sequence watermark. All physical updates must
still use the established observation primitives.

Result tag 0 carries a value/sequence; the typed value can be unavailable (255).
Tag 1 means `UnknownAttribute`, and tag 2 means `Busy`; neither carries a sequence
or value. Unavailable is an actual observation state and is distinct from a read
failure, false, or zero. A retry uses a fresh read token and samples again; relays
and owners do not cache/replay a state value. Duplicate copies of the same read
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

The common forwarder has a 2,048-byte compile-time ceiling, including maintained
adjacent links, read/discovery records, four call paths, and eight delta duplicate keys. Owner
invocation history has a 768-byte ceiling. Appliance and transport storage are
static; argument decoding uses bounded stack storage. No allocator is enabled.

Host checks cover native signature/reference rejection, mutex contention, exact
command replay, changed-token content rejection, delayed observations, errors
after physical changes, grouped-report ordering, instance filtering, unavailable
values, hard eviction, relayed results/deltas, scope/expiry, and numeric wire bounds.
State-read checks also cover unavailable/busy/unknown results, delayed replies versus newer deltas, two-relay delivery, shared-path
correlation/eviction, and preserving pending changes.
Ordinary and observer C6/nRF images build. These are host/build checks: this new
path has not been exercised on boards, and RF investigation remains paused.
