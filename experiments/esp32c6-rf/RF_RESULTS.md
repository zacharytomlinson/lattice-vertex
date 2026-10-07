# C6 RF investigation results

Temporary RF investigation history. Kernel integration instructions remain in
[the hardware radio check](../../src/kernel/radio/TESTING.md). All `target/` capture
paths below are relative to the repository root and remain unchanged.

### Missing-reply investigation

The first pair run demonstrates transport activity but contains unanswered
probes. Their cause remains unresolved. Inspection found that the C6 fixture
enabled `esp-println/critical-section`: its lock masks interrupts across the
entire formatted write, including USB FIFO waits. The C6 MAC handler rearms RX
after frame completion, so delaying that interrupt creates a potential receive
gap. The fixture now leaves that logging feature disabled and runs its reporter
below the radio task. Normal execution has one printing task; startup printing
precedes the scheduler and panic printing halts execution.

The rebuilt images add these fixture-owned diagnostics:

- `probes_sent`: successfully completed probe transmissions, separate from replies.
- `pre_peer_probes`: probes sent before any valid peer traffic was observed.
- `pending_probe`: a probe whose reply window is still open.
- `unanswered`: a closed window with no matching reply, for a probe sent after
  observing peer traffic. A probe window closes when the next probe is attempted;
  it is counted once and does not trigger a retry.
- `unmatched_replies`: valid replies that do not match the open probe, including
  late or duplicate replies.
- `rssi_dbm`: most recent received-frame RSSI where the port supplies it (C6).
- `rx_abort_events` / `rx_abort_last`: observed hardware RX-abort interrupts and
  their latest port-native reason. For C6, reason 3 is CRC, 4 is invalid length,
  and 5 is filtering. Hardware abort masks are unchanged; this is not an exhaustive
  count of RF errors. `receive_errors` still counts errors returned by the receive
  API, which previously hid this distinction.

This comparison keeps channel 15, maximum configured TX power, disabled MAC ACKs,
and the probe cadence unchanged. The logging fix is a supported timing correction,
not a confirmed explanation of the earlier missing reply. Reflash both boards
and capture the new counters to determine whether losses persist. C6 output now
includes its MAC address so the two streams can be identified.

### Follow-up: PHY timing mismatch

The user-reported run after the logging fix still had one closed, unanswered
probe on `9C:CC:01:40:67:98` (20 probes sent, 19 replies, one pre-peer probe,
no pending probe). `E8:F6:0A:FB:EF:E0` reported 22 probes and 22 replies, with
zero unanswered. Both reported RSSI around -70 to -72 dBm and no IRQ-reported
RX aborts. The logging fix did not resolve the entire issue.

Auditing the linked `libbtbb.a` found that its initializer sets baseband TX-on
delay to 29 and MAC TX-on to 35, but our imported driver then overwrote only the
baseband delay with 50. The port now preserves the vendor-initialized settings.
This is a concrete initialization mismatch; its effect on the missing reply and
reported RSSI remains to be measured.

The new C6 image prints an `RF mac=...` line alongside each statistics sample.
Expected readback for this pinned library and test channel is `hop=23` (channel
15), `power_index=15`, `bb_tx_on=29`, `mac_tx_on=35`, `rx_on=50`, and
`txrx_switch=127`. The timing values are reported in their native register/API
units. The power index checks programming, not actual radiated power.

`crc`, `sfd_timeout`, `filter`, and `no_rss` come directly from hardware error
counters, rather than the software RX-abort callback. The fixture reads these registers without writing the counter-clear register
or changing abort masks. Capture both the RF and statistics
lines to distinguish the programmed timing, frame errors, and missing round trips.

### Result after preserving PHY timing — 2026-10-06

User-supplied readbacks confirm `hop=23`, `power_index=15`, `bb_tx_on=29`,
`mac_tx_on=35`, `rx_on=50`, and `txrx_switch=127` on both boards.

| Final reported measurement | `9C:CC:01:40:67:98` | `E8:F6:0A:FB:EF:E0` |
| --- | --- | --- |
| Kernel tick | 51,150 | 50,149 |
| Probes sent / matching replies received | 22 / 22 | 23 / 23 |
| Pre-peer probes | 0 | 1 |
| Unanswered / pending / unmatched replies | 0 / none / 0 | 0 / none / 0 |
| Received / transmitted | 46 / 46 | 44 / 44 |
| Interrupt wakeups | 92 | 88 |
| API errors / evictions / ignored frames | 0 / 0 / 0 | 0 / 0 / 0 |
| Last RSSI | -64 dBm | -69 dBm |
| Hardware CRC / SFD timeout / filter / no-RSS counters | 0 / 51 / 3 / 0 | 0 / 72 / 1 / 1 |

All originated probes have matching replies in this capture, including the one
sent before peer contact on `E8:F6:0A:FB:EF:E0`. RSSI spans -64 to -70 dBm in the
submitted excerpts, and SFD-timeout counters increase. Those hardware counters
are not associated with peer addresses or probe tokens, so they cannot be counted
as additional lost diagnostic probes. They also show why zero API-error counters
alone are insufficient for assessing RF behavior.

The timing readback is verified and this short run has no missing round trips.
It does not prove that the timing change caused the earlier missing reply or
settle the received-signal/range concern. The retained vendor reference capture
used channel 15 at **0 dBm**; a controlled **20 dBm** reference run at the same
positions would be the relevant RF comparison. Its ELF, bootloader, and partition
table remain in ignored `target/esp32c6-radio/reference/` with recorded hashes.

### Fixed-power PHY initialization comparison

All further C6 runs, including reference firmware, stay at 20 dBm. Three units
and all pairings have already produced similar received signal levels. The
combo-PHY comparison changed PHY setup only; it retained channel 15, power index 15, vendor
timings, packet format, and cadence.

Inspection of the linked dependencies found `esp-phy` 0.2.0 skips
`phy_init_param_set(1)` on the C6. The C6 metadata identifies it as a combo-radio
part. The retained SDK reference calls that selector before PHY initialization,
and [upstream PR #5776](https://github.com/esp-rs/esp-hal/pull/5776) restores the
selector along with explicit Wi-Fi RX control. The C6 port now calls the selector
before PHY calibration and `phy_wifi_enable_set(0)` afterward for its 802.15.4-only
use. Not starting the Wi-Fi stack alone did not establish the PHY receiver state.

The subsequent hardware capture still shows weak received signal and missing
round trips:

| Reading | `E8:F6:0A:FB:EF:E0` | `9C:CC:01:40:67:98` |
| --- | --- | --- |
| Kernel tick | 38,114 | 43,126 |
| Probes sent / matching replies received | 17 / 16 | 18 / 16 |
| Pre-peer probes | 0 | 2 |
| Unanswered / pending / unmatched replies | 1 / none / 0 | 1 / none / 0 |
| Last RSSI | -66 dBm | -66 dBm |
| Hardware CRC / SFD timeout / filter / no-RSS counters | 1 / 37 / 0 / 0 | 0 / 41 / 0 / 1 |

Both boards retain the expected channel/power/timing register values and report
no API errors or evictions. Each has one unanswered probe sent after peer contact.
The CRC error is not associated with a particular peer or probe. These samples
do not establish whether a probe or its reply was lost. The combo-PHY change has
not resolved the reported behavior.

No calibration blob has been substituted: the Rust PHY input block differs from
the older SDK reference, and differences between library revisions need to be
understood before changing those inputs. These diagnostics now live in the experiment tree.

### Vendor reference at 20 dBm

`reference.py` drives the retained ESP-IDF v5.5.1 CLI image. It checks its
recorded hashes, flashes both boards, verifies channel 15 and 20 dBm on each,
and sends 20 frames in each direction. The frames match the Lattice broadcast
probe layout and size, with no requested MAC ACK or retries. Source addresses
are distinct test addresses. This measures raw-frame delivery and directional
RSSI; host pacing differs from the kernel workload and there are no application
replies. It does not test the kernel task/notification path.

Close both serial monitors and leave board positions and orientations unchanged.
From the repository root, with `espflash` and Python `pyserial` installed:

```sh
python3 experiments/esp32c6-rf/reference.py --flash /dev/ttyACM0 /dev/ttyACM1
```

The helper restores the current `src/port/esp32c6/tests/target/` release `radio`
image after the run, including on a test error. It attempts restoration on both
boards independently and reports failure with the exact restore command and a
nonzero exit. Build the `radio-probe` image first if that artifact is missing or
outdated. `--dry-run` verifies artifacts and prints commands without opening the
boards; omitting `--flash` assumes both already run the vendor CLI and leaves
their firmware in place. TX power is fixed, not a command-line option.

Results and per-board serial logs go under the printed
`target/radio-reference/<UTC timestamp>/` directory. `run.json` records image
provenance and the new run's settings; the embedded reference provenance also
contains its historical lower-power capture, which is not this run's setting.
`results.json` reports matched payloads and minimum/median/maximum RSSI by
direction. The retained vendor artifacts are local ignored files under
`target/esp32c6-radio/reference/`, not distributed with the kernel source.

The helper writes the retained bootloader, partition table, and application
binaries verbatim at their SDK offsets. The first hardware attempt showed that
`espflash flash --flash-freq 80mhz` rewrites the bootloader frequency header from
0x30 to 0x3f (and its digest); the boards then failed in the ROM loader. Restoring
the original bootloader bytes restored startup. A successful offline image build
had not caught that incompatibility. Separate binary writes preserve the original
SDK header and avoid erasing NVS/calibration between partitions.

### Vendor comparison result — 2026-10-06

Capture: `target/radio-reference/20261006T151816.374472Z/`. Board A is
`9C:CC:01:40:67:98`, board B is `E8:F6:0A:FB:EF:E0`. Both vendor CLIs confirmed
channel 15 and 20 dBm before transmission. Every frame had a matching peer payload
and a sender TX-completion report, with no requested MAC ACKs or retries.

| Direction | Peer received / sent | RSSI minimum / median / maximum |
| --- | --- | --- |
| A → B | 20 / 20 | -71 / -68 / -64 dBm |
| B → A | 20 / 20 | -76 / -69 / -65 dBm |

The vendor image reproduces similarly weak reported RSSI without Lattice's Rust
radio driver or kernel. That makes a Lattice-specific fault insufficient to
explain the signal readings. It does not establish calibrated received power,
radiated TX power, antenna performance, or the cause of occasional unanswered
kernel probes. Twenty host-paced frames per direction are not a range or
loss-free-delivery proof. Shared RF setup, PHY/calibration behavior, and the
reported RSSI itself remain relevant; this result does not diagnose bad boards.

Both boards were flashed back to the current Lattice probe successfully.
The following 45-second serial capture (`lattice-A.log`, `lattice-B.log`, and
`lattice-results.json` in the same directory) confirms both are running it:

| Reading at tick 44,131 | A: `9C:CC:01:40:67:98` | B: `E8:F6:0A:FB:EF:E0` |
| --- | --- | --- |
| Probes sent / matching replies received | 19 / 19 | 20 / 20 |
| Pre-peer probes | 0 | 1 |
| Unanswered / pending / unmatched replies | 0 / none / 0 | 0 / none / 0 |
| API errors / evictions | 0 / 0 | 0 / 0 |
| Sampled last-RSSI range | -70 to -63 dBm | -70 to -63 dBm |
| Hardware CRC / SFD timeout / filter / no-RSS counters | 0 / 83 / 1 / 0 | 1 / 68 / 0 / 0 |

Register readbacks remain hop 23, power index 15, and timing 29/35/50/127.
The logged RSSI is the most recent reception at each reporting tick, so repeated
samples are not independent packet measurements. This short follow-up has no
missing round trips; it does not invalidate the earlier loss captures or identify
a fix. No kernel code was changed for this comparison.

### Wi-Fi control — 2026-10-06

The [standalone Wi-Fi fixture](wifi/README.md) uses channel 4, 20 MHz
bandwidth, and the verified 20 dBm Wi-Fi setting (80 in quarter-dBm API units).
Driver logs showed completed scans with zero APs. Explicit combo-PHY/RX enable
calls did not change that. A dedicated AP plus a separate passive receiver,
initialized directly in their final modes, also produced no peer samples.
The captures are under `target/wifi-comparison/20261006T160109.535175Z/`,
`20261006T160248.780962Z/`, and `20261006T160505.175363Z/` respectively.
These are failed Wi-Fi controls, not measured low RSSI values.

An independent firmware control used the official
[MicroPython ESP32-C6 v1.29.0 image](https://micropython.org/download/ESP32_GENERIC_C6/),
with its ESP-IDF Wi-Fi implementation, commanded over USB serial. Both radios
reported 20.0 dBm through `WLAN.config('txpower')`; the active AP reported channel
4. Each board took a turn as the sole AP, with the other scanning. All eight scans
in each direction returned zero networks, including zero neighboring APs. Thus
the failure is not confined to the Rust Wi-Fi fixture, but there is still no valid
Wi-Fi RSSI comparison or antenna diagnosis. `-128` in the survey logs is an empty
result sentinel, not a measured signal.

The official binary, download URL/hash, serial logs, results, capture script, and
complete 8 MiB flash backups are retained under
`target/wifi-comparison/micropython/`. Both backups were restored after that run.
A subsequent phone check used `9C:CC:01:40:67:98` advertising
`lattice-rf-check` at channel 4 / 20 dBm, with the other board held in its
bootloader. The phone initially did not show the SSID, then detected it; the user
reported a strong/full or nearly full bar indication. This is qualitative
independent reception of that board's Wi-Fi transmission, not a dBm reading or
validation of the C6 receive path. It makes a gross antenna/transmit-path failure
less likely for that board without resolving the 802.15.4 signal readings.

A further C6 receiver check against this already-advertised AP returned one empty
scan, then lost its USB serial connections. The automatic backup restoration
failed (A missing, B reporting a broken pipe). Both USB devices subsequently
returned. Their complete original flash backups were restored successfully,
and serial capture confirmed both running the Lattice radio probe again, with
one matching round trip each and last RSSI -65/-66 dBm. Recovery logs are
`restored-A.log` and `restored-B.log` in the MicroPython capture directory.
Do not interpret the interrupted Wi-Fi run as an RF result.

Host Wi-Fi scanning previously disrupted the laptop's internet connection.
The old host-scanning runner was retired; the current runner uses USB serial
only. No laptop Wi-Fi scanning or configuration was used for these follow-ups.

### Register assay — 2026-10-06

The [C6 register assay](RF_ASSAY.md) maps the hardware controls to the
datasheet, TRM, vendor headers/HAL, our writes, and raw readbacks from both boards.
The extra snapshot now lives behind the experiment image's `rf-registers` feature.

The initial assay found that the driver enabled PAN interfaces 0 and 1 when only 0
is requested; its average-ED setter changes the sampling-rate field instead of
the vendor's averaging bit; and all four Wi-Fi-power LP clock selectors are set.
The current RSSI trailer offset matches the vendor path, and channel/power-index
readbacks remain 23/15 (channel 15 / requested 20 dBm). The assay changed no
production register writes. Both boards were restored to the regular probe.

The subsequent [correction and verification](RF_ASSAY.md#corrections-and-verification)
fixed all three discrepancies. Live readbacks show PAN mask 1, one ED sample/us
with average accumulation, and the unused Wi-Fi-power LP clock gated/deselected.
Channel 15 and requested 20 dBm remain fixed. The 60-second run still reported
two/four unanswered post-contact probes, with one pending on A, and sampled RSSI
from -84 to -71 dBm. The regular probe left on both boards includes the fixes;
the overall RF problem is still open.

### OpenThread control and board profile — 2026-10-06

The [OpenThread reference comparison](openthread/README.md) retains the
pinned firmware source/configuration, successful raw-frame and Thread results,
live register differences, flash/calibration captures, and nanoESP32-C6 maker
profile check. The C6 port now explicitly enables the modem ETM clock, and the
radio fixture runs at 160 MHz. The final 40-second probe test answered 17/17 and
18/18 round trips; RSSI remained -66 to -61 dBm. Both boards run that corrected
regular probe. No host Wi-Fi scanning was used.

### GPIO3/GPIO14 antenna-switch trial — 2026-10-06

The user suggested that the unidentified boards might copy the XIAO RF switch
wiring. [Seeed's board documentation](https://github.com/Seeed-Studio/wiki-documents/blob/docusaurus-version/sites/en/docs/Sensor/SeeedStudio_XIAO/SeeedStudio_XIAO_ESP32C6/XIAO_ESP32C6_Getting_Started.md)
specifies GPIO3 LOW to enable switch control and GPIO14 LOW for the internal
antenna. That wiring is not established for these boards. The maker schematic
for the visually similar nanoESP32-C6 is also not proof of their actual wiring.

The experiment's optional `antenna-gpio-test` feature saves both pins' register
state, drives GPIO3 LOW, waits 100 ms, and drives GPIO14 LOW before radio
initialization. At tick 30,000 it restores the saved output latches, output
enables, pin configuration, output selection, and IO_MUX words, asserting exact
readback. It never selects an external antenna. Regular probe builds only report
these GPIO registers; ordinary kernel builds contain neither the report nor the
trial at the time of this capture. The GPIO diagnostics are now confined to the
experiment image; the kernel test binaries no longer include them. Build the
trial from `experiments/esp32c6-rf`:

```sh
cargo +stable build --release --locked --features antenna-gpio-test --bin radio --target-dir target/antenna-gpio
```

Both connected boards were tested at channel 15, 20 dBm, and 160 MHz CPU. A
25-second regular-probe baseline preceded the trial, followed by approximately
30 seconds with both pins driven LOW and 25 seconds after pin restoration.

| Receiver | Baseline RSSI median (range), dBm | Both LOW | Restored |
| --- | --- | --- | --- |
| A: `9C:CC:01:40:67:98` | -63 (-69 to -63) | -63 (-69 to -62) | -68 (-69 to -62) |
| B: `E8:F6:0A:FB:EF:E0` | -68 (-68 to -61) | -68 (-69 to -62) | -67 (-68 to -62) |

RSSI samples are the last received value reported each second, not independent
per-packet measurements. The baseline finished with 10/10 and 11/11 matching
round trips. Before pin restoration the trial had 12/12 and 13/13, reaching
23/23 and 25/25 by its final sample; no closed unanswered or pending probes were
reported at these boundaries. These short runs do not establish range.

On both boards the GPIO enable mask changed from `0x0` to `0x4008`, the masked
output remained `0x0`, and both IO_MUX words changed from `0xa00` to `0x1800`.
The driven values persisted through radio initialization. Restoration returned
all saved registers exactly, including enable `0x0` and IO_MUX `0xa00`.

This sequence produced no consistent RSSI improvement: both driven-LOW medians
equal their baseline. It does not establish whether another switch or different
wiring exists. No antenna GPIO policy was added to the kernel or platform port.
Both boards were reflashed with the regular probe after verified pin restoration.
The final serial check confirmed their original GPIO state and 3/3 matching
round trips each. Trial and ordinary builds passed; the ordinary ELF contains
neither the GPIO diagnostic strings nor the shared probe symbols.
Raw captures, image hashes, and phase summaries are retained under
`target/antenna-gpio-test/20261006T195736Z/`.
