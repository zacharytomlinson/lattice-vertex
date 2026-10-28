# Continuous native control

`peer::control` owns live absolute scalar targets for an existing native
transformer. Appliance adapters supply the same getter and setter used by ordinary
native invocation. They acquire no stream callback, networking method or loop.
The caller chooses a one-argument transformer and an observed state from its
surface. Signature checks establish the scalar type; the caller must choose an
absolute setpoint operation rather than an incremental action such as `Toggle`.

## Ownership and lifetime

The resident peer owns four receiver sessions and, with `host-link`, four source
listeners. Each source and receiver retains one latest target. A newer sequence
replaces pending intent; an older retry cannot restore it. The native execution
owner can retarget a prepared control call until its worker starts. A running call,
ordinary invocation and program invocation cannot be replaced. Capacity and native
hardware admission remain local; this adds no exclusive ownership of an appliance
transformer or arbitration between separate controllers.

Open binds caller/nonce, transformer and observed state to a receiver ticket:
durable boot incarnation, slot and generation. Repeated Open is idempotent while
that session remains resident. Open also reserves a path record at each relay;
a successful reply establishes the pinned adjacent directions. The receiver requires the existing committed program
configuration incarnation; a host companion can originate without storing programs.
These identities detect stale sessions; they are not authentication credentials.

Target records current intent, not a promise to execute every intermediate sample.
Finish seals the newest sequence and waits for its native result. Close abandons
unsealed pending/prepared intent; it does not cancel a running call or a sealed
final target. Unsealed sessions expire after 30 seconds without target/control
activity. Sealed work remains until completion, followed by a 30-second result
retention interval. Slots advance generation on reuse and do not wrap into old
tickets. A receiver or relay reset requires a new session and fresh caller intent.
The compact handle is the caller's fresh 64-bit nonce, not a recycled slot index.
The Linux client obtains it from the OS random source; active collisions are
rejected. Clients must not deliberately reuse a nonce for another stream. This
is probabilistic session identity, not authentication or a cryptographic guarantee.

USB disconnect discards source intent and invalidates its listeners. Already
delivered work has its receiver-owned lifetime; disconnect cannot instantly revoke
RF packets or a native call. A stale feedback listener stops further publication
and abandons the receiver's unsealed pending work. No automatic reopen/replay occurs.

## Delivery and feedback

The host endpoint accepts asynchronous targets independently of its ordinary
request/reply mailbox. The source coalesces targets before transmission and replaces an
obsolete pending sample when newer intent arrives. The destination
pushes addressed progress when reception, native completion or its acquired state
changes. Progress itself receipts target delivery; no per-target routed reply is needed. Neither path depends on receipt
of best-effort outward delta broadcasts; actual registered deltas retain their
normal publication and program-activation semantics.

Progress distinguishes newest received target, most recent completed target/native
status, and the observed state's value/watermark. Completed sequences can skip
superseded targets. A snapshot need not equal the requested target: native success
does not prove physical convergence, and another writer or physical change may
affect the observation. Snapshots coalesce; an unread terminal fault is retained
ahead of later snapshots. Finish establishes a final native outcome plus the latest
observation, or the host reports that its outcome remains unconfirmed.

Open uses existing owner-directed discovery/read routing. Its successful return
pins a path in `peer::forwarding::streams`: caller/owner, previous/onward neighbors,
full receiver ticket and the stream nonce. Four fixed path records are available
per vertex. Exhaustion refuses setup instead of evicting another active stream.
The first successful branch is retained; setup retries cannot silently move it.
Finish/Close use the full-ticket lifecycle envelope along those same directions.

Samples, progress, progress receipts and interruptions use the compact bound path.
Relays check the incoming neighbor and forward directly to the stored next neighbor;
no end-to-end address lookup, per-sample read record, broadcast exploration or path
repair occurs. Missing paths return Stale toward the sender; fault packets never
answer another missing-path fault. Pending Open bindings drop premature data until
the setup reply arrives, allowing setup retries to complete. A fault/close retires
the path; idle records expire after 30 seconds. Streams inherit the network context
from setup, so their compact frames do not repeat its founder/channel fields.

Each source and receiver can await one progress/receipt; waiting for one stream
does not block another stream or the reverse direction. A 200 ms timeout
uses bounded backoff: target retries from 50 to 400 ms, progress from 100 to 1600 ms.
New input supersedes an older pending target immediately. Sealed finite native work
refreshes unchanged progress every ten seconds to retain its path while running.
The source reports interruption after 30 seconds without receiver progress. This
keeps an unavailable peer from retaining a source stream forever; it does not cancel
a running hardware call. The existing task wakeup deadlines serve these intervals.

## Wire and bounds

Setup uses inner command **24** in the existing Program service envelope. It replaces
unbound Open tag 19, which is rejected. Finish/Close retain inner tags 21/22. The
compact RF records use outer LVT v7 opcodes 25–28; each is prefixed by the existing
four-byte magic/version. Their handle is the nonce established by Open.

| RF opcode | Fields after opcode | Lattice payload bytes including magic/opcode |
| --- | --- | ---: |
| 25 Target | handle `u64`, sequence `u32`, raw argument | 18–25 |
| 26 Progress | handle `u64`, revision `u32`, progress | 28–41 |
| 27 Receipt | handle `u64`, revision `u32` | 17 |
| 28 Fault | handle `u64`, status `u8` | 14 |

Argument width/type comes from the bound transformer's signature. The receiver
rejects wrong widths and invalid Boolean representations. Sequence numbers do not
wrap into reuse. The full boot/slot/generation ticket remains in setup/lifecycle
records and local state; it is not transmitted with every sample. Receipt/progress
metadata is transport-owned, leaving only the argument in the application payload.

The addressed MAC header currently adds 21 bytes, excluding the two-byte FCS. Thus
a U8 target uses 39 MAC bytes before FCS, compared with the preceding transport's
75. Every message fits one RF frame. Radio frames still need their MAC header and
checksum; binding does not imply unframed RF bytes or zero-copy memory access.

Host v4 USB kinds 8/146/147 retain their existing typed target/progress/fault records
and identities. The USB transport is not the compact RF frame format. Open uses the
new setup tag through existing host kind 7; clients and all relays must use the new
wire definitions. Older unbound setup is rejected, not silently adapted. Full-ticket
Target/Feedback codecs remain for USB records/internal conversion; they are rejected
as routed Program requests. The public host API needs no change.

Compile-time ceilings remain 2 KiB for the control owner and 2 KiB for the host
mailbox. Four forwarding records add at most 448 bytes; the total forwarder ceiling
is 2912 bytes. Native workers, MAC delivery and routing during setup are reused.
No per-stream task, heap or unbounded sample queue is allocated.

## Client and validation

`lattice-tools/peer` exposes `Client::control`, then `set`, `progress`, `finish` and
`close` on its control handle. `set` waits for the local USB write, not a radio
round trip. The CLI accepts typed values on stdin and finishes at EOF. The tablet
can consume this shared client; tablet UI integration is subsequent work.

Component checks partition ordering, type/identity conflicts, prepared-call
replacement, capacity, expiry, final retention, route retirement, stale feedback,
and lost request/reply recovery with newer intent. Wire tests enforce encoding and
frame bounds; host/client checks keep snapshots separate from actual changes.

On 2026-10-22, the existing C6 and Nordic OTA-appliance fixtures exercised both
directions. Each accepted a 200-target burst and 100 targets at 50 Hz, then reported
the final native result and matching acquired scalar. USB write medians were
0.07–1 ms; finish-to-confirmation took 94–223 ms in those four runs. The final
lifecycle-cleanup build repeated 50 Hz input both ways with 135/196 ms final
confirmation. Feedback was
coalesced, not one event per input. These are one-hop software-backed state checks,
not physical dimmer timing, multi-hop performance or guaranteed response bounds.
Saved definitions and commissioning configuration were retained. Captures and the
temporary measurement harness live outside this repository.

The compact path has host checks for reserved/active capacity, stable forwarding
after ordinary route expiry, wrong-neighbor rejection, stale/closed bindings,
setup retries and exact frame widths. C6/Nordic release builds pass. A subsequent
live repeat completed 100 targets at 50 Hz in each direction, with final native
success and matching acquired state (194/188 ms finish confirmation in those runs).
A 1.2-second native-call fixture also completed both ways while hardware-event
acquisition remained available; running work needed no 10 ms service scan.

The network wait includes control retry, contact/idle expiry and heartbeat deadlines.
Native completion/context release and cache-read readiness notify the service.
These checks used software-backed states and one-hop paths; they do not establish
physical dimmer timing, multi-hop radio reliability or tickless CPU behavior.
