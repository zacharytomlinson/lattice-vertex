# Firmware updates

This document owns the implemented update package, receiver, activation policy,
and known limits. See [the architecture](ARCHITECTURE.md) for system ownership,
[tool procedures](../tools/README.md) for packaging and transfer, and
[port checks](../src/port/TESTING.md#update-checks) for reproducible validation.

The current updater replaces a combined kernel/appliance image. That is accepted
for the current functional-kernel work. [Independent kernel updates](ARCHITECTURE.md#independently-replaceable-kernel-and-appliance-code)
remain a later objective and are not implemented. The package format, boot layouts
and checks below describe whole-image support.

Replacing the combined image remains separate from installing an automation.
An ordinary supplying peer, including a computer with a USB companion, transfers the package through existing
peer forwarding. No tablet, nucleus, router, Internet connection, or permanent
update coordinator is required. Receivers do not yet serve retained images onward.

## Ownership

| Owner | Responsibility |
| --- | --- |
| Common kernel | Authenticate, stage/resume, verify, choose activation, and confirm local startup |
| Platform port | Bound flash access, select the trial, report boot state, confirm, and reset |
| Bootloader | Recover installation, boot the trial, restore an unconfirmed image |
| Appliance | Initialize hardware and indicate ongoing physical work that cannot be interrupted |
| Supplying peer | Send the immutable signed package and its measured costs |

The resident peer task owns the receiver and flash. One candidate uses bounded
alignment storage and hash state, with no image-sized RAM buffer, updater task,
allocator, or additional routing table. The transfer journal owns download
progress; boot metadata owns trial/confirmation/reversion. Ordinary builds reserve
12 KiB for the radio task, update builds 24 KiB. Acquisition and native-call
tasks separately reserve three 4 KiB stacks; port fixtures enable stack guards.

Trust comes from the provisioned release key, independently of the supplying
peer. Node-admission authentication is separate. Current packages update only
applications, not bootloaders, partition maps, or release keys. Provisioning and
flash layouts belong to the [Nordic](../src/port/nrf52840/boot/README.md) and
[C6](../src/port/esp32c6/boot/README.md) boot components.

## Implemented signed transfer

Enable `firmware-update` with `nrf52840-update` or `esp32c6-update`.
`kernel::start_with_updates` receives port storage and a build-provisioned
`update::package::Policy`: trusted Ed25519 public key, platform, layout revision,
appliance family, running release, and surface fingerprint. Startup checks that
fingerprint against the actual encoded appliance surface. The sender cannot
choose the receiver's policy.

The package is a 192-byte numeric header followed by the image:

| Byte range | Field |
| --- | --- |
| 0–7 | `LVUP`, format u16, platform u16 |
| 8–27 | Layout u32, appliance family 16 bytes |
| 28–59 | SHA-256 of the current/candidate exported surface |
| 60–71 | Release u64, image length u32 |
| 72–103 | Image SHA-256 |
| 104–127 | Six u32 costs: erase/page, write/byte, verify/byte, activation, swap/byte, longest pause |
| 128–191 | Ed25519 signature over bytes 0–127 |

Integers are little-endian. Per-byte costs use Q24.8 microseconds; other costs use
integer microseconds. `0xffffffff` means unknown. Package identity is the first
16 bytes of SHA-256 over the complete signed header. The strict Ed25519 verifier
uses neither allocation nor precomputed tables. Signature, target, surface, size,
and newer-release checks precede flash mutation. The full digest and target
vectors/header must match before readiness.

| Request | Bounded effect |
| --- | --- |
| Header | Assemble four independently acknowledged 48-byte ranges |
| Prepare | Erase at most one page or commit the journal header |
| Data | Accept at most 48 image bytes with package identity and offset |
| Finish | Advance verification; phase 7 (`Verifying`) requires another Finish |
| Status | Read retained progress without advancing it |

Busy means no flash progress occurred for that request. The largest radio frame
is 124 bytes before FCS, within the 125-byte MAC limit. LVT v7 opcodes 16/17 and
host v4 kinds 5/0x85 reuse existing correlation, reverse paths, and routes. Relays
retain no image. Older peers cannot relay unsupported records; receivers without
the service return Unsupported. Wire records live in
[the shared codec](../src/kernel/peer/wire/README.md).

## Durable progress and image lifecycle

```text
accepted offer → staging → verifying → ready → trial → confirmed
                                                 └→ unconfirmed reset → revert
```

The running image remains available during transfer and verification. A rejected
or interrupted download leaves it running. A verified candidate may wait
indefinitely; an offer is not a reboot deadline.

Each port reserves an 8 KiB journal. The signed header becomes recoverable only
at its commit marker. Append-only offset/complement records checkpoint completed
4 KiB pages or the final image length. Torn records are skipped, never rewritten.
Restart revalidates the header, erases the unfinished page and remaining staging
area, and returns the committed offset. Re-offering the same package resumes
there; a different identity starts a new candidate. Accepted bytes may exceed
committed bytes. Neither a MAC ACK nor an accepted range proves durability.

Duplicates compare against accepted data or return retained progress without
reprogramming the same flash word. A partial final write stays within the reserved
slot. Keys, identity, and persistent appliance data remain outside image slots.
Persistent-data migrations must remain readable by the old image during trial or
wait until confirmation. Reverting firmware cannot undo a physical operation.

The journal is consumed before trial selection so reversion cannot silently
reselect the downloaded candidate. Selection reuses the verified digest while the
port exclusively owns unchanged candidate flash, and rechecks the target prefix.
It is not a second physical-corruption scan.

Explicit SelectTrial, Confirm, and Reset use attached-host kinds 6/0x86 or local
kernel APIs; there is no radio reboot control. Unknown-cost candidates require
explicit activation. Confirm remains subject to local startup checks.

## Preserving Lattice configuration

Installed programs and commissioning data need a protected nonvolatile region
outside firmware erase/write/swap extents. A durable, erase-aligned firmware-region
limit is sufficient; the kernel can write its own retained configuration beyond
that boundary. New images must fit the established limit. Independent kernel and
appliance binaries are not required for this protection.

Layout 3 now reserves 16 KiB: Nordic `0xE9000..0xED000`, C6
`lattice_config` at `0x212000..0x216000`. Nordic's active/DFU extents shrink to
364/368 KiB; its loader descriptor at `0x2EFC0` must match before the port permits
flash access. C6 validates the data partition's type, alignment and disjointness.
Both ports lend this region through their existing flash owner. An unconfirmed
trial can read it but cannot rewrite it. Configuration is excluded from journal
and image erase/write/swap operations.

The shared [configuration store](../src/kernel/storage/README.md#local-configuration)
maintains two committed copies, detects incomplete/corrupt writes and retains the
previous valid copy. The [program loader](../src/kernel/program/README.md) now
validates and reconstructs installed definitions from it; definitions remain
inactive until the separate execution/admission layer is implemented.
Layout 3 needs coordinated loader/partition/application provisioning. No boards
have been provisioned or power-cut-tested with these new boundaries yet.

## Handle compatibility

Surface handles now derive from permanent appliance binding names. Reads, native
invocation and deltas resolve those numeric references against the current local
declarations. LSF v2 also removes addresses from mathematical records, so a
compatible rebuild that only relocates code/data preserves the surface fingerprint.
Duplicate binding names/references are rejected. Changing a physical meaning
requires a new name; type equality alone is not compatibility evidence.
The LSF v1-to-v2 transition changes the fingerprint. An existing v1 release policy
will reject a v2 package as incompatible; use coordinated provisioning for this
format transition, not a claim of transparent OTA compatibility.

Unattended updates require preserved exported handles and meanings, compatible
wire behavior, and rollback-compatible persistent data. Packaging compares actual
surface dumps; a developer's compatibility flag is insufficient. The signing
pipeline must also establish that those dumps belong to the supplied binaries.
If compatibility cannot be established, automatic deployment still needs a future
migration/invalidation mechanism. Stable references do not authorize incompatible
type, operation or physical-behavior changes.
Empty-appliance boot fixtures do not establish this for arbitrary appliances or
provide installed-program migration.

## Measured costs

Signed costs come from release dry runs on the target, including the longest
unavoidable staging pause. They are planning estimates, not execution guarantees.
Communication uses the shared directional
[time per acknowledged payload byte](../src/kernel/peer/wire/README.md#communication-cost):

```text
transfer service = path cost × remaining transmitted payload bytes
local staging    = erase/page × pages + write/verify costs × image bytes
activation       = fixed startup/selection cost + swap/byte × actual swap extent
```

Count protocol envelopes as payload, and progress/control traffic in its own
direction. MAC retries are already in the measured marginal cost. Unknown cost
stays unknown. Contention, deferral, and overlapping work prevent treating these
sums as a guaranteed completion timestamp. There is no separate bulk estimator.
See [measurement procedures](../tools/README.md#measuring-a-release).

## Paced staging and incremental verification

`update::staging` admits one bounded operation on the resident peer task. Recent
ordinary work must settle for its estimated duration. Afterward the kernel leaves
an equal estimated service interval, at least six ms at the current PHY, for normal
work. Post-operation timing avoids counting flash-stalled ticks as radio service.
Pending native calls, ordinary reads, captured/in-progress frames, and registered
changes take precedence. Update return paths do not block one another.

A refused operation returns Busy with unchanged progress; it creates no queued
flash job. Status, completed operations, and identical written ranges remain
readable. The supplying tool retries immutable update requests with ten-ms Busy
backoff and a thirty-second per-request deferral limit. Continued activity stops
that attempt; the package can be offered later. Native mutations are never retried
by this mechanism.

Native-call admission holds updates through task completion. A conflicting flash
step can make admission report Busy. Acquisition uses a quiet exclusion hold:
it does not restart activation deferral merely by reading unchanged hardware. Physical work continuing after a call uses
`update::hold_updates(&context)?` when both flash pauses and reboot must wait, or
`hold_activation` when only reboot must wait. These are physical-work guards, not
appliance-defined update schedules. Explicit maintenance controls are overrides.

Finish targets one ms of estimated hash work, capped at 1,024 image bytes per
request. Storage reads through one aligned 256-byte buffer, retaining only hash
state and cursor. Zero cost still respects the byte cap; unknown verification
cost uses 256-byte steps. Other unknown costs receive only the minimum service
interval and cannot qualify for automatic activation.

Reset preserves durable bytes but discards partial verification; hashing restarts
from the beginning. Writes, erases, and resume invalidate the cached digest. Only
the complete matching digest and port prefix validation produce Ready. The kernel
uses `verify_step`; the synchronous `verify_image` helper is for explicit local
port callers. Updater clients must decode Verifying and continue Finish rather
than treating every successful response as Ready.

Pacing gives normal work opportunities; it cannot shorten an indivisible erase
or predict a new physical event. Tighter latency bounds need port support such as
partial erase or execution from RAM and measurements of the actual workload.

## Local activation policy

`update::activation` uses complete known signed costs and actual swap extent:
the whole Nordic active slot, zero on C6. Unknown or excessive outage estimates
leave the image ready for explicit maintenance. The upper bound is ten minutes;
overflow never becomes a free cost.

Quiet must last at least the estimated outage or five seconds, whichever is
longer, plus an address/time-mixed backoff below two seconds. Local changes, native
access, reads, calls, deltas, and their relaying restart the wait. Formation/link
maintenance and update records are not user work. Outstanding ordinary return
paths defer selection until answered or expired. This does not predict future
user activity or timed program obligations; continued activity can defer forever.

Long physical work retains a counted activation/update hold. Busy means admission
has closed: do not begin that operation. Dropping the guard records activity and
wakes the peer task. Automatic selection closes new holds atomically, then rechecks
activity and pending deltas. An `Appliance::with` after commitment waits for reset
or reopening after failed selection.

A one-hop proposal precedes departure by two seconds. Existing neighbor records
retain it. A departing neighbor takes precedence; simultaneous proposals use the
smaller radio address. New activity cancels the proposal. A final leaving notice
precedes selection; failed TX or captured/in-progress reception postpones it.
There is no network-wide lock or new scheduler.

LVT v7 opcode 18 carries a nine-byte notice after the network tag: attempt u32,
phase u8 (proposed=0, leaving=1, cancel=2), and outage milliseconds u32. Cancellation
must match. Duplicates do not extend expiry; stale proposals cannot downgrade a
leaving notice. Expiry includes outage, notice time, and five seconds of slack;
the neighbor can remain retained beyond ordinary link expiry until then. Ordinary
firmware understands notices without an updater; older firmware ignores them.
Loss or partitions can still permit simultaneous updates. No alternate relay path
is guaranteed. A ready measured candidate bounds pending-policy receive waits to
the actual quiet/backoff or notice deadline; no periodic activation scan is required.

## Watchdog-backed startup confirmation

`storage::startup` confirms after storage/appliance initialization, successful
read-only program reconstruction, an advancing kernel tick, and a completed local
radio energy measurement. The receiver-incarnation checkpoint waits until trial
confirmation permits configuration writes; execution then waits for that checkpoint.
Startup confirmation needs no neighbor,
packet exchange, or successful network formation. This establishes local startup,
not arbitrary appliance correctness. The kernel writes confirmation once. An
uncertain failure stops watchdog feeding rather than retrying changed metadata;
the loader resolves the state after reset. Manual Confirm cannot bypass readiness.

Both ports use a nominal thirty-second watchdog, fed by peer-task progress rather
than tick/radio interrupts. It remains active after confirmation, when a later
stall resets the confirmed image instead of reverting. Long finite appliance calls run in separate kernel tasks; peer-task progress
continues to feed the watchdog. This watchdog does not impose a call deadline or
prove that arbitrary hardware functions eventually return. The separate local USB
boot fixture keeps explicit confirmation to exercise unconfirmed reversion.

Both ports require successful reconstruction of saved program definitions before
the common kernel confirms a trial. The Nordic loader arms WDT after its full-slot
swap and before trial entry. It uses the persistent resulting boot state, because `prepare_boot` reports entry
state even after reversion. The peripheral remains enabled until reset. The C6
loader hands off RTC WDT and the port rearms it at initialization. **C6 has a known
handoff gap:** esp-hal 1.1.2 disables watchdogs inside `esp_hal::init`, before the
port rearms one. A stall in that interval is uncovered. Keep port initialization
directly after HAL initialization/resource setup. Exact watchdog configuration
belongs to the [Nordic](../src/port/nrf52840/boot/README.md) and
[C6](../src/port/esp32c6/boot/README.md) boot notes.

Watchdog-capable loaders require provisioning, and layout 3 additionally changes
the configuration reservations. Application packages cannot install those loaders. Factory Nordic
recovery and C6 ROM recovery remain available; provisioning changes no eFuses.

## Validation scope and remaining work

[Maintained checks](../src/port/TESTING.md#update-checks) cover local storage,
signed staging/resume, activation holds, startup confirmation, and watchdog
reversion. Both ports have physical lifecycle checks and one-hop signed transfers
in both directions. Historical captures and timing observations are archived in
the external lab; they are not release cost certificates.

Open limits are physical interruption during a flash write/swap, shorter
indivisible flash pauses, reference rebinding across incompatible surfaces, remote
running-release reporting, and serving retained packages from a vertex. Current
host upload completion proves verified staging, not which release subsequently
runs. Local diagnostics established boot outcomes in the hardware checks.
Version admission is relative to running firmware, not secure boot or
hardware-enforced anti-rollback. No multi-hop hardware or arbitrary physical
appliance guarantee follows from the empty-appliance fixtures.

Deferred independent kernel OTA needs separate image and RAM ownership, a
stable appliance ABI, component-specific package admission, and boot/trial/revert
integration. Existing authenticated transfer, staging and local activation work
can be assessed for reuse. Their current tests do not establish that the appliance
binary remains untouched or runs correctly after a kernel-only update.
