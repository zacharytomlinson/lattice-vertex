# Common kernel peer checks

This checks `kernel::start` → resident radio task → `kernel::peer`, without
installing a replacement workload. The optional `peer-observer` board feature
supplies an authored test surface, a passive callback, fixed counters, and a
low-priority reporter. It was previously named `peer-discovery`.
Configuration, broadcasts, packet parsing, and directed responses belong to the
ordinary kernel. The RF probe remains separately selectable through `radio-probe`;
the two fixture features are mutually exclusive.

From `src/port/esp32c6/tests`:

```sh
cargo +stable build --release --locked --features peer-observer --bin radio --target-dir target/discovery
espflash flash --port /dev/ttyACM0 target/discovery/riscv32imac-unknown-none-elf/release/radio
```

Select the actual board port. For nRF52840, build the same feature in its board
directory and use SWD or the XIAO USB path in its
[README](../../port/nrf52840/tests/README.md). No ESP dependency or
separate protocol implementation is required on Nordic.

The kernel searches channels 11–26, using PAN `0x4c56` and factory-derived extended
addresses for discovery. C6 stays at 20 dBm; Nordic uses its supported 8 dBm.
Both peers use identical firmware with no formation/join parameter. Existing
residents supply their network's founding address and channel. With no resident
answer, two energy surveys rank channels by their highest observed energy, then
computed channel rendezvous exchanges proposals before formation. Only channels
with at least two successful measurements can be selected locally. Proposals
heard from other starters use a deterministic founding-address/channel tie-break.

The PAN is a common discovery filter and the founding address a network tag.
Neither authenticates a peer. Startup currently adopts the first established
network it hears; ownership selection between nearby deployments is not
implemented. [Surface reads](EXPOSURE.md) now use bounded exploration and learned
owner directions. Compact arrivals and owner queries now cross relays without sending
complete surfaces.
[Native invocation, state reads, and registered deltas](INVOCATION.md) now share that runtime.
Compiled-fragment installation/activation and authenticated admission remain unimplemented.

The on-air discovery profile is:

| Field | Encoding |
| --- | --- |
| MAC | 2006 data frame, compressed PAN, extended source, sequence byte; ACK requested only for extended unicast |
| Discovery destination | PAN `0x4c56`, short broadcast `0xffff` |
| Presence destination | PAN `0x4c56`, requesting peer's extended address |
| Proposal destination | Broadcast solicits an answer; directed reply carries the candidate's preferred record |
| Discovery payload | `4c 56 54 06` (`LVT`, version 6), then kind 0 (discover), 1 (present), or 2 (propose) |
| Present/propose fields | Founding address: 8 bytes little-endian; channel: 1 byte |
| Size before hardware FCS | 20 bytes discovery; 35 bytes directed presence/proposal; 29 bytes broadcast proposal |

The same envelope includes arrival (3), surface read (4), surface part (5),
find owners (6), owner found (7), invocation (8), result (9), state changed (10),
read state (11), state value (12), link exchange (13), read links (14), and link
entry (15),
specified in [the exposure format](EXPOSURE.md#read-envelope). Reads are driven
by an interested consumer; discovery never triggers full-surface publication.

Startup scans actively: each channel gets a 4,096-us energy window, a discovery
request, and 60–90 ms of listening. Formation has no fixed control channel.
Candidates use 40-ms local intervals, visiting channels with a prime stride for
51 intervals and staying on their preferred channel for 17. A round advances the
start index. The sequence is calculated from a few integers, without a table or
clock synchronization. It runs at most three rounds (nominally 8.16 seconds),
ending earlier upon established presence or a matching directed proposal.
Proposals are sent twice per interval at staggered offsets to reduce collisions.
These are local discovery intervals, not a graph execution schedule.

The sequence follows the jump/stay construction from
[Enhanced Jump-Stay Rendezvous](https://doi.org/10.1109/LCOMM.2013.071013.131029),
also described explicitly in [this paper's section 3](https://qiangshenghua.github.io/papers/cocoon15.pdf).
With 16 physical channels, the prime is 17; stride is `preferred_channel - 10`,
and the virtual seventeenth channel maps to the preference. Our bounded startup,
radio timing, and proposal exchange do not inherit an end-to-end convergence
guarantee from that channel-sequence result. Established peers stay on their
selected channel; later arrivals find them by scanning. No merge protocol for
established networks is included.
The network record is volatile; reboot searches again. The founding peer is not
required once another resident retains its record. Established nodes do not yet
migrate channels in response to later interference.

After selection, a compact arrival can cross relays. Periodic discovery stays
link-local and introduces candidates for the bounded link-maintenance exchange.
There is no automatic owner query. [Link traversal](LINKS.md) lets interested peers
follow references from one reachable contact; `FindOwners` remains explicit.
Presence ends that local exchange; there is no
echo loop, neighbor registry, graph retry, or allocation. Unsupported versions,
foreign PAN/payloads, self frames, wrong recipients, truncation, trailing payload,
wrong resident channel, invalid network tags, and wrong addressing for a kind
are rejected. Candidates advertise and answer broadcasts while visiting channels,
with their proposed operating channel in the payload. That proposal
may name another channel; established presence must match the receive channel.
Versions 1–5 are intentionally incompatible with the current version 6.
Host checks cover wire vectors, rejection boundaries, channel selection, proposal
ordering, inherited network replies, response termination, and rendezvous overlap:

```sh
cargo +stable test --locked --lib kernel::peer --no-default-features --features port-test,use-mutexes -- --test-threads=1
```

`DISCOVERY` logs show the local address, last peer that sent directed presence,
selected channel/PAN, founding address, `joined`, proposal count, `agreement_channel`, per-channel
peak energy, scan errors, and request/response counters. Channel/founder remain
zero until selection. `joined=true` means an established resident answered;
false after selection means the node completed the proposal process. Zero peer
means no accepted presence yet. `agreement_channel` records where a matching
directed proposal allowed early settling; zero means this was not the exit path.
`owner`, `owner_via`, `owner_surface_bytes`, `owner_hops`, and `owner_solicited`
describe the last accepted compact owner notice. `owner_notices` counts notices,
not distinct owners or complete membership. RSSI belongs to the immediate radio
neighbor, not necessarily to a relayed owner. Owner query/reply and arrival-forward
counters are separate from the local formation discovery counters.
The counters are not a peer registry or packet-loss
estimator: a startup broadcast can precede the other peer, and one broadcast may
receive multiple peers' presence responses. All counters live in the fixture.

Build without either fixture feature in a separate `target/normal` directory to
produce ordinary firmware. It retains discovery but has no observer/probe crate,
test event callback, statistics, or recurring serial logs. Pairing this ordinary
image with an observed peer checks that test code is not implementing discovery.
Ordinary port-test firmware supplies `surface::EMPTY`; the observer image supplies
a switch or dimmer definition bound to its native instance. See [the exposure checks](EXPOSURE.md#checks-and-current-status) for
the range/forwarding checks. Two passive observers do not initiate reads.

## Validation scope

The current version-6 kernel has host/build validation, plus a two-Nordic
attached-host `ReadLinks` run. The Linux client obtained the attached resident's
neighbor and read that neighbor's reciprocal link and end-of-list over radio.
The test exposed a back-to-back discovery/link-offer conflict; maintenance now
sends one solicitation per interval, and both links established after that change.
USB reopen and software DFU entry also worked. The subsequent host v2 path read
the maintained dimmer's complete surface, invoked its typed native transformer,
and read back registered state. Wrong argument types and out-of-range values
returned distinct native errors. The fixture is software-backed; this is not
physical actuator verification or a range/loss-rate test. Some reads timed out;
no mutation was automatically retried. A final read after closing the diagnostic
console succeeded too. Earlier console-closed and post-reflash reads timed out,
including a rerun with that console open; console state alone was not established
as their cause. One concrete restart defect was fixed: host-originated reads now
seed their counter from the host token instead of deliberately reusing read 1
while another node retains that identity. A regression check covers that case.
The radio stack reserves 8 KiB for the measured ~5 KiB entry frame plus callers,
callees, and exception context. Neither that allocation nor the diagnostic USB
polling cleanup established the cause of every earlier timeout.

The host v3 stream has also delivered registered power changes and a subsequent
single-state level change to Linux after native calls, without `ReadState` requests.
An idle `watch` blocks on USB input and makes no serial writes. The same check had
surface-read timeouts and incomplete two-change bursts; no overflow marker was
reported. This proves the event path, not complete delivery or the cause of a
missing delta. Ring overflow, reply priority, reconnect isolation, and coalesced
reply/event decoding have deterministic checks.

Native calls, relayed
surface/current-state reads, reciprocal link maintenance, guided traversal, owner
discovery, scored alternate directions, bounded invocation replication, and deltas
have deterministic host checks; their
complete tablet-to-appliance path still needs hardware validation. Earlier images
exercised channel formation and peer exchange on C6 and Nordic. Those results do
not establish the current wire version's end-to-end behavior or radio range.

The subsequent transport investigation identified the repeated second-delta loss
as a half-duplex turnaround failure. The common transport now receives between
sends, preserves captured bytes for ordinary dispatch, and bounds completion of
an already-started frame before interruption. See the [radio validation](../radio/TESTING.md#receive-turns-in-the-resident-peer)
for the controlled comparison and remaining losses. Forwarding and native replay
semantics are unchanged; no retry engine was added.

Historical board captures and RF comparisons live in an external lab archive.
They are not dependencies of these checks. RF investigation remains paused.
