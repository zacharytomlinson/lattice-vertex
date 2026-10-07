# Common kernel discovery check

This checks `kernel::start` → resident radio task → `kernel::peer`, without
installing a replacement workload. The optional `peer-discovery` board feature
adds only a passive callback, fixed counters, and a low-priority reporter.
Configuration, broadcasts, packet parsing, and directed responses belong to the
ordinary kernel. The RF probe remains separately selectable through `radio-probe`;
the two fixture features are mutually exclusive.

From `src/port/esp32c6/tests`:

```sh
cargo +stable build --release --locked --features peer-discovery --bin radio --target-dir target/discovery
espflash flash --port /dev/ttyACM0 target/discovery/riscv32imac-unknown-none-elf/release/radio
```

Select the actual board port. For nRF52840, build the same feature in its board
directory and load with SWD as described in its README. No ESP dependency or
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
implemented. No surface, state mutation, or compiled-fragment messages exist yet.

The on-air discovery profile is:

| Field | Encoding |
| --- | --- |
| MAC | 2006 data frame, compressed PAN, extended source, sequence byte, no requested ACK |
| Discovery destination | PAN `0x4c56`, short broadcast `0xffff` |
| Presence destination | PAN `0x4c56`, requesting peer's extended address |
| Proposal destination | Broadcast solicits an answer; directed reply carries the candidate's preferred record |
| Payload | `4c 56 54 02` (`LVT`, version 2), then kind 0 (discover), 1 (present), or 2 (propose) |
| Present/propose fields | Founding address: 8 bytes little-endian; channel: 1 byte |
| Size before hardware FCS | 20 bytes discovery; 35 bytes directed presence/proposal; 29 bytes broadcast proposal |

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

After selection, discovery repeats every 2–3 seconds. Presence ends the exchange; there is no
echo loop, neighbor registry, graph retry, or allocation. Unsupported versions,
foreign PAN/payloads, self frames, wrong recipients, truncation, trailing payload,
wrong resident channel, invalid network tags, and wrong addressing for a kind
are rejected. Candidates advertise and answer broadcasts while visiting channels,
with their proposed operating channel in the payload. That proposal
may name another channel; established presence must match the receive channel.
Version 1 from the fixed-channel test is intentionally incompatible.
Host checks cover wire vectors, rejection boundaries, channel selection, proposal
ordering, inherited network replies, response termination, and rendezvous overlap:

```sh
cargo +stable test --locked --lib kernel::peer --no-default-features --features port-test
```

`DISCOVERY` logs show the local address, last peer that sent directed presence,
selected channel/PAN, founding address, `joined`, proposal count, `agreement_channel`, per-channel
peak energy, scan errors, and request/response counters. Channel/founder remain
zero until selection. `joined=true` means an established resident answered;
false after selection means the node completed the proposal process. Zero peer
means no accepted presence yet. `agreement_channel` records where a matching
directed proposal allowed early settling; zero means this was not the exit path.
The counters are not a peer registry or packet-loss
estimator: a startup broadcast can precede the other peer, and one broadcast may
receive multiple peers' presence responses. All counters live in the fixture.

Build without either fixture feature in a separate `target/normal` directory to
produce ordinary firmware. It retains discovery but has no observer/probe crate,
test event callback, statistics, or recurring serial logs. Pairing this ordinary
image with an observed peer checks that test code is not implementing discovery.

## Formation validation status — 2026-10-07

Five focused host tests pass. The C6 ordinary and observed discovery images and
the nRF52840 observed discovery image build/link. The C6 hardware check below
exercises energy detection, channel changes, joining, and simultaneous formation.
Nordic energy detection remains build-verified only. The older 2026-10-06 result
validates fixed-channel version 1 only.

The hardware check procedure is:

1. Flash both boards with version 2, then power only A. Capture the scan readings,
   selected channel/founding address, and any scan errors.
2. Power B. It should report `joined=true` and retain A's channel and founding
   address, followed by directed presence responses in both logs.
3. Remove A, leave B running, then return A. A should recover the same record from
   B; this checks that the founder is not a required discovery service.
4. Start both together and compare selected records. A split formation is a
   concrete race to investigate, not evidence of successful convergence.

Use USB serial only. ED scans run on the MCUs; the laptop Wi-Fi adapter is never
scanned or reconfigured. Diagnostic scan readings and counters stay in the
hardware fixture; the selection survey and resident network record belong to the
kernel.

The schedule check covers every preferred-channel pair, all 17 peer start indices,
and all 68 whole-interval phase offsets against a reference start. It retains an
overlap even when one physical channel other than the selected preferences is
unavailable. This includes channel 15. It checks computed channel opportunities;
fractional timing, clock drift, and delivery under real interference are separate.

### Current computed-rendezvous implementation

Capture: `target/channel-formation/20261007T200804Z/`, with raw logs, image hashes,
flash output, and phase summaries. Both C6s ran the same image at 20 dBm.

| Phase | Result |
| --- | --- |
| A alone | Selected channel 24 after surveying; no scan errors |
| B started later | Adopted A's channel 24 and founding address; presence exchanged both ways |
| Founder held in bootloader | B retained the selected record and continued running |
| Founder returned without test features | B's received-presence count increased from 7 to 12 and sent-presence count from 5 to 11 |
| Founder returned with logging | A reported `joined=true` and recovered the same record from B, which remained running through tick 41,080 |
| Simultaneous starts | A received an agreeing directed proposal on channel 16; both settled on operating channel 12 and founding address `9ecc01fffe406798` |

The simultaneous run ended with 6/8 received-presence responses and correct peer
addresses. A reported `agreement_channel=16`; B subsequently joined the established
record. All final phase samples reported zero scan and receive/transmit API errors.
The ordinary image contains no test workload, observer, or antenna-test code.
Both boards were left running the new observer image. Nordic builds pass; its
hardware path remains untested. This was not an RF-jamming test: the blocked-channel
check above concerns schedule opportunities, while the hardware run establishes
real agreement on a non-designated channel and subsequent participation.

The earlier fixed formation channel is removed from the implementation. Channel
generation, proposal handling, and selection remain together in `kernel::peer`;
ports still own only measurement and radio I/O, and test counters remain outside
the ordinary kernel. No sequence table, new allocator, or extra task was added.

### Earlier shared-channel implementation

Historical capture: `target/channel-formation/20261007T194749Z/`. It retains raw serial
logs, phase summaries, stable port identities, image hashes, and flash output.
The reproducible bench runner is retained at `target/channel-formation/run.py`.
C6 transmit power remained 20 dBm throughout.

| Phase | Result |
| --- | --- |
| A alone, B held in bootloader | A selected channel 21, whose peak was -98 dBm, the lowest of the 16 measured channel peaks; no scan errors |
| B started later | B reported `joined=true`, channel 21, and A's founding address; directed presence exchanged both ways |
| A held in bootloader | B retained channel 21 and the same network record |
| A returned with ordinary firmware | B's received-presence count increased from 7 to 12 and sent-presence count from 4 to 10; A had no test features or recurring logs |
| A returned with observer firmware | A reported `joined=true` and recovered the same record from B; B remained running through tick 40,085 |
| Both started together | Both selected channel 12 and founding address `9ecc01fffe406798`, identified one another, and exchanged directed presence |

In the simultaneous run, both logged `joined=false`: both completed proposal
settling with the same selected record, rather than discovering an already
established resident. Final received-presence counters were 3 and 4. All final
phase samples reported zero scan, receive-API, and transmit-API errors. Startup
request totals include every scan channel and periods when the other board was
offline; they are not lost-packet counts. These short runs do not establish
convergence under arbitrary interference or partition, nor resolve the earlier
RF range concern. Both boards were left on the observer image.

Earlier captures at `20261007T194210Z` and `20261007T194512Z` exposed split
formation. Opening the native USB monitors restarted both boards in those runs;
the revised runner keeps the continuing peer's port open across founder resets.
That intermediate correction used a shared formation channel and kept proposals
visible during the final sweep. It passed the tests above but imposed an unwanted
dependency on channel 15. It is superseded by the current computed-rendezvous path.

## C6 result — 2026-10-06

Historical result for version 1, before automatic channel formation.

Capture: `target/peer-discovery/20261006T204057Z/`. Image SHA-256 hashes, stable USB
port identities, flashing output, and each phase's raw serial output are retained.
The pair used the same image and independently selected channel 15/PAN `0x4c56`.

| Board | Factory MAC | Extended address |
| --- | --- | --- |
| A | `9c:cc:01:40:67:98` | `9ecc01fffe406798` |
| B | `e8:f6:0a:fb:ef:e0` | `eaf60afffefbefe0` |

During the initial 18-second observed run, A's final sample showed 8 discovery
broadcasts and 8 directed presence responses received; B showed 9 and 9. Each
reported the other's extended address. A answered 9 requests and B answered 8.
Ignored frames, receive errors, and transmit errors were zero in both samples.
These counters establish the exchange during this short capture, not RF range
or a general delivery guarantee.

B was then flashed with ordinary firmware built without either test feature.
A stayed running: its uptime advanced from tick 17,018 to 31,032, received
requests increased from 9 to 15, and received presence responses from 8 to 13.
B printed startup only. The ordinary ELF contains no `lattice_radio_test`,
discovery-reporting, or antenna-trial symbols/strings. This establishes that
ordinary firmware both originates and answers discovery without a fixture handler.

After B's observer image was restored, A continued through tick 45,046 and B
reached tick 11,012 with 6 broadcasts and 6 presence responses received. Both
still identified the correct peer with zero reported API errors. Counts across
the reflashing intervals are not loss measurements because B was offline.
Both boards were left running the discovery observer image.

Both host framing/response tests pass. C6 ordinary, observed discovery, and RF
probe images build; the nRF52840 observed discovery image builds using
`cargo +nightly build -Z build-std=core --release --locked --features peer-discovery --bin radio`.
Nordic hardware execution remains untested. Naming/ownership review leaves one
transport owner, one common discovery handler, and passive fixture-owned counters;
no appliance networking API or neighbor registry was introduced.
