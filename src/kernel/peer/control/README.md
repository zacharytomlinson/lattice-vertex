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
that session remains resident. The receiver requires the existing committed program
configuration incarnation; a host companion can originate without storing programs.
These identities detect stale sessions; they are not authentication credentials.

Target records current intent, not a promise to execute every intermediate sample.
Finish seals the newest sequence and waits for its native result. Close abandons
unsealed pending/prepared intent; it does not cancel a running call or a sealed
final target. Unsealed sessions expire after 30 seconds without target/control
activity. Sealed work remains until completion, followed by a 30-second result
retention interval. Slots advance generation on reuse and do not wrap into old
tickets. A receiver reset requires a new session and fresh caller intent.

USB disconnect discards source intent and invalidates its listeners. Already
delivered work has its receiver-owned lifetime; disconnect cannot instantly revoke
RF packets or a native call. A stale feedback listener stops further publication
and abandons the receiver's unsealed pending work. No automatic reopen/replay occurs.

## Delivery and feedback

The host endpoint accepts asynchronous targets independently of its ordinary
request/reply mailbox. The source coalesces targets before routing and cancels an
obsolete outstanding target exchange when newer intent arrives. The destination
pushes addressed progress when reception, native completion or its acquired state
changes. Target replies can also carry progress. Neither path depends on receipt
of best-effort outward delta broadcasts; actual registered deltas retain their
normal publication and program-activation semantics.

Progress distinguishes newest received target, most recent completed target/native
status, and the observed state's value/watermark. Completed sequences can skip
superseded targets. A snapshot need not equal the requested target: native success
does not prove physical convergence, and another writer or physical change may
affect the observation. Snapshots coalesce; an unread terminal fault is retained
ahead of later snapshots. Finish establishes a final native outcome plus the latest
observation, or the host reports that its outcome remains unconfirmed.

The existing owner-directed read routing carries these exchanges. Each peer has
one outstanding control exchange; dispatch rotates through receiver feedback and
source intent. A 200 ms exchange timeout uses bounded retry backoff: targets from
50 to 400 ms, feedback from 100 to 1600 ms. New input supersedes its older retry
immediately. The resident task's receive deadline includes control deadlines;
native work uses the existing call tasks and update holds. There is no new RTOS
task, heap, polling message, or per-appliance scheduler.

## Wire and bounds

`wire::control` contains numeric records inside the existing Program service
envelope, alongside program and clock requests. LVT v7/host v4 are unchanged.
These are inner command tags, not the retired outer LVT opcodes 19/20.

| Inner tag | Request | Encoded bytes |
| --- | --- | ---: |
| 19 | Open: epoch, nonce, transformer, observed state | 33 |
| 20 | Target: ticket, sequence, typed scalar | 21–28 |
| 21 | Finish: ticket, final sequence | 19 |
| 22 | Close: ticket, nonce | 23 |
| 23 | Feedback: nonce, ticket, revision, progress | 38–51 |

A ticket occupies 14 bytes. Progress occupies 11–24 bytes depending on observed
state availability/type. A state reply adds epoch/type/revision in its payload and
uses the ordinary reply header's offset/generation for the ticket. Each request
and reply fits one existing RF frame; no stream payload fragmentation is added.

USB kind 8 carries current intent without a request reply. Kinds 146/147 carry
unsolicited progress/faults. All three use token zero and separate stream identity.
Open/Finish/Close use the existing request/reply path. The host mailbox has four
latest targets and four control events, separate from eight ordinary changes.
Compile-time ceilings are 2 KiB for the entire control owner and 2 KiB for the
entire host mailbox, including its pre-existing storage. Native workers and route
records are reused. No per-stream task or unbounded sample queue is allocated.

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
