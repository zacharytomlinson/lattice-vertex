# Shared numeric wire definitions

`lattice-peer-wire` owns the declared-width scalar/argument codec used by peer
frames, surface literals, and attached-host messages. The current consumers use LVT v7, host v4, and LSF v2. Values and integer domains
come from `lattice-ir`; protocol status values live here so the host need not invent
a second interpretation of native dispatch or state-read failures. The registered
`StateChange` record is also shared by RF deltas and host events.

Native invocation messages use the existing scalar argument bounds and statuses
0–5. Experimental LVT v7 opcodes 19/20 and statuses 6/7 belonged to the removed
callback-fragment prototype; they are now rejected and reserved against semantic
reuse in this wire version. `program` now owns LPD v1 records and installation
commands/replies used by LVT opcodes 21/22 and host kinds 7/0x87. See the
[program component](../../program/README.md) for installation, receiver-reserved
handoffs and execution limits. Their runtime remains in the kernel.

`clock` supplies numeric commissioning profiles, recurring Gregorian/civil rules,
temporal predicate encodings and reference messages. It carries no live clock,
deadline task or hardware dependency. [Local time/solar](../../time/README.md) owns
those runtime responsibilities.

`control` defines latest-target sessions and addressed native/observed progress.
Open now uses inner tag 24 in the Program service envelope; old unbound Open
tag 19 is rejected. Compact RF targets/progress/receipts/faults use outer opcodes
25–28 on a bound path, with raw target bytes interpreted by the bound signature.
Finish/Close retain inner tags 21/22. The optional host codec adds asynchronous target
kind 8 and progress/fault kinds 146/147. Each record fits the existing frame bounds.
The [control owner](../control/README.md) implements their runtime and lifetime.

The optional `host` feature adds the bounded local packet records and COBS stream
codec described in [the host interface](../host/README.md). This replaces the former
`lattice-host-link` crate under `host/wire`: scalar encoding now has both radio and
USB consumers, so its owner is the common peer wire crate. The crate contains no
kernel execution, platform support, solver, or interpreter.

`update` owns the bounded header/data/preparation/status request grammar and
progress reply shared by radio forwarding and USB. It also defines local-only
update maintenance controls. Signature verification, journals, flash access, and
activation decisions stay in `kernel::update` and the platform ports. The optional
host codec adds kinds 5/6 and 133/134 without altering existing host-v4 records.
See [the updater](../../../../docs/FIRMWARE_UPDATES.md#implemented-signed-transfer)
for the signed package and byte layout.

Update phase 7 (`Verifying`) retains partial hash progress. A successful Finish
may report this phase; send another Finish until Ready. Busy leaves progress
unchanged and permits a later immutable-request retry. These additions keep the
28-byte progress width and LVT v7 / host v4 envelopes. Updater consumers must use
the updated phase decoder; ordinary read/call/change records are unaffected.

`update::Absence` is the fixed nine-byte one-hop planned-outage notice. It carries
an attempt token, phase, and bounded relative duration; LVT v7 opcode 18 encodes
it after the network tag. It has no routed or attached-host request equivalent.

## Communication cost

Communication cost is normalized per unit of delivered information. The shared
`lattice-peer-wire::cost::TimePerByte` represents microseconds per acknowledged
MAC payload byte, in unsigned Q24.8 fixed point. Both MCU ports use the same
meaning, independent of CPU clock. `ratio` compares two known costs;
`estimate_us(quantity)` applies any byte quantity to the same value. Ten bytes
and ten thousand bytes do not select different models. Energy is not silently
mixed into the time unit.

The resident transport measures each addressed send from its first attempt
through its successful receive turn or exhausted attempts. This includes MAC
header/FCS airtime, ACK waits, retry backoff, and the mandatory receive opportunity;
the denominator counts the acknowledged payload once, excluding MAC header/FCS.
Failed sends add service time and zero acknowledged bytes. Remote application
execution and idle time between sends are not measured. The first implementation
uses the kernel tick clock (1 ms on the supported ports), so these are quantized
service estimates, not fine-resolution PHY timestamps or hard deadlines.

Eight existing neighbor records own the measurements. Samples are aged by halving
the accumulated evidence after roughly 4 KiB of acknowledged payload; 60 seconds
without new samples makes the cost unknown. Four transient samples hand timing
results from the radio task's sends to those records. No sample history array,
allocator, network-wide cost table, or separate task is introduced. Frame overhead
is amortized over the observed packet-size mix; changes in that mix or contention
change the estimate as new observations arrive. A consumer counts the actual MAC
payload bytes its operation will transmit, including its protocol envelopes.

An owner's reply starts with zero remaining path cost. Each relay adds its own
outgoing-link cost toward the replying neighbor and advertises that sum on the
reverse reply. The recipient adds its own outgoing link to that suffix. Reverse
link cost and request/reply processing latency are not substituted for forward
cost. Route records retain the suffix separately, allowing fresh first-hop
measurements to change ranking without copying or rediscovering the entire path.
This is an additive serialized service estimate; overlapping transmissions or
future traffic cannot be inferred as a guaranteed wall-clock completion time.

Unknown is encoded as `0xffffffff`, not zero. Zero denotes an empty/local path.
Unknown components or arithmetic overflow leave the estimate unknown. Known
paths rank by their normalized cost, including when a longer route is cheaper
than a direct link. Hop limits still bound traversal. Link enumeration exposes
both the normalized outgoing cost and the existing round-trip diagnostic; RTT
is no longer the routing objective.

The OTA service preserves these quantities in its transfer replies. Measured local
costs drive staging admission and automatic activation deferral. Firmware-local
flash/verification/boot costs remain distinct from communication time.

Program command 29 removes one owner-local connection set by its definition offset
and generation. Reply status 17 means Removed; Persisting remains unconfirmed.
This leaves other connection sets sharing its computation intact. See the
[program component](../../program/README.md#explicit-removal) for lifecycle semantics.
LPD v1 input is unchanged; the kernel's LPG v4 storage format is internal to its
persistent image and supports vertex-local computation sharing.
