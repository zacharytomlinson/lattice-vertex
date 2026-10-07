# ESP32-C6 RF register assay

Inspected 2026-10-06 on the two revision-v0.2 C6s. TX remains configured at
20 dBm. The initial assay below records the configuration before correction.
The discovered errors were subsequently fixed and retested; see
[Corrections and verification](#corrections-and-verification) for current behavior.
Source links now point to the corrected implementation. All captures used USB
serial; the laptop's Wi-Fi was not used.

## Source authority and limits

| Source | What it establishes |
| --- | --- |
| [C6 datasheet v1.5][datasheet], overview, §4.3.4, §6.3 | Shared antenna/RF architecture and specified 802.15.4 performance |
| [C6 TRM v1.2][trm], chapters 7, 8, 12 | GPIO routing, system clocks, resets, and power management |
| [C6 register header][mac-reg], [C struct][mac-struct], [common HAL][mac-hal], [C6 HAL constants][c6-hal], ESP-IDF v5.5.1 | Wireless register layout and the vendor driver's actual accesses |
| [Modem clock implementation][clock], [SYSCON][syscon] and [LPCON][lpcon] headers | Modem gates, clock-state maps, and source selection |
| [PHY initialization][phy-init] and [antenna guide][antenna] | Vendor initialization/calibration entry points and external switch routing |
| Local `esp32c6` 0.23.2 PAC, `esp-phy` 0.2.0, `esp-wifi-sys-esp32c6` 0.2.0, and our port | What this firmware actually implements; these are checked against the sources above |

The public TRM's chapter list contains no detailed Wi-Fi/Bluetooth/802.15.4 MAC
or analog RF chapter. It is not a complete PA/LNA/AGC register specification.
Wireless headers provide more detail, but their names are not uniformly reliable:
the C6 MAC header and its C struct disagree about the ED fields. In that case,
trace the HAL's compiled access rather than treating a generated name as truth.

The datasheet describes a shared antenna and analog RF path with separate
protocol basebands/MACs. For 802.15.4 it specifies channels 11–26 at 2405–2480 MHz,
250 kbit/s O-QPSK, up to 20 dBm TX, typical -104 dBm sensitivity at 1% PER, and
typical +8 dBm maximum received signal at 1% PER. These are specification values,
not measurements of these boards. Phone bars establish qualitative Wi-Fi
reception only; they are not a calibrated link-budget measurement.

Downloaded PDFs, pinned vendor headers/source, and source hashes are retained
locally under `target/rf-assay/`. Live observations are in
`target/rf-assay/20261006T175238Z/{board-A.log,board-B.log,registers.json,run.json}`.
A is `9c:cc:01:40:67:98`; B is `e8:f6:0a:fb:ef:e0`. These ignored artifacts are not
kernel dependencies. The decoded JSON retains every captured address/raw word.

## Baseline discrepancies

### 1. PAN mask writes the wrong bits

At `0x600a3004`, interfaces 0–3 occupy bits 28–31. Both Espressif descriptions
and the current Rust PAC agree. The original
[`set_multipan_enable_mask`](../../src/port/esp32c6/radio/registers.rs)
instead cleared/set a mask shifted by 29. That preserved interface 0 and lost
the fourth bit beyond the 32-bit word. It is an inherited implementation error,
not a disagreement between the current PAC and vendor layout.

Both boards read `CTRL_CFG = 0x32080000`: `(value >> 28) & 15 == 3`, enabling
interfaces **0 and 1**, although the PIB requests only interface 0. Interface 0
has our proper address; interface 1 retains PAN/short address `ffff` and a zero
extended address. This broadens the enabled filtering surface unintentionally.
It does not by itself explain a received-power reading.

The nested address-register offsets were also checked and **do match**: each
16-byte bank contains short address, PAN, extended low word, extended high word.
The cluster's generated description lists those names in another order, but
the actual Rust structure/accessors use the correct layout.

### 2. The average-ED request changes sampling rate

The vendor HAL's `ieee802154_ll_set_ed_sample_mode()` assigns the C struct's
single-bit field at **bit 13**. Its sampling-rate field occupies **bits 11–12**.
The macro header and PAC instead label bits 11–12 `ED_SAMPLE_MODE` and bit 13
`DIS_ED_POWER_SEL`. Our original `.ed_sample_mode().bits(1)` therefore changed rate rather
than selecting the vendor's average mode.

At `0x600a3054`, the readbacks are A `0x009b48c4`, B `0x009948c4`:

| Decode using the vendor's operational C struct | Both boards |
| --- | --- |
| Sampling-rate selector, bits 11–12 | 1 |
| Sample mode, bit 13 | 0: maximum, although our code requests average |
| CCA mode, bits 14–15 | 1: energy detection |
| Signed CCA threshold, bits 0–7 | -60 |

The differing bits 16–23 are an ED result (-101/-103 as signed bytes), not the
received packet's RSSI. CCA behavior and RSSI interpretation must remain separate.
The retained SDK reference also configures a -60 threshold; that threshold is
not a new difference discovered between the two implementations.

### 3. All four Wi-Fi-power low-frequency sources are selected

At `0x600af00c`, both boards read `0x0000000f`: slow RC, fast RC, main crystal,
and 32 kHz crystal selection bits were all set, with divider zero. The original
[`init_clocks`](../../src/port/esp32c6/radio/clocks.rs) explicitly set all four.
The vendor clock-selection path first deselects every source, then selects one.

This is a concrete initialization divergence. The available public description
does not establish what combination wins electrically when all are asserted.
It is not yet a demonstrated cause of weak RSSI, and no source was changed during
the initial assay. The follow-up traced and corrected that dependency below.

## Hardware control inventory

Addresses below are absolute or written as an explicit base plus offsets. Hex values are baseline readbacks before the fixes;
unless noted, both boards agree. Field interpretations use the references above,
not solely the PAC labels. Volatile status observations are not an atomic snapshot.

| Control / observation | Address or interface | Existing configuration / observation | Relevance and limits |
| --- | --- | --- | --- |
| CPU source and divisors | PCR `0x60096000 + {0x110, 0x118, 0x11c, 0x120}` | `28010200`, `100`, `300`, `0`; source selector 1 = PLL | TRM §8.2.4.4 requires PLL-sourced CPU operation for Wi-Fi/BLE. Readback is a selection, not an oscillator-accuracy measurement. |
| Modem APB availability | PCR `0x60096108` | `1` | Bus clock enabled; register access is available. |
| Modem clock state maps | SYSCON `0x600a980c`; LPCON `0x600af020` | `64666600`; `66660000` | Maps select gating by PMU state. ZB/BT maps are 6 here; vendor initial maps are 4. Both include ACTIVE, so the difference alone does not imply a missing active-mode clock. |
| Selected PMU modem codes | PMU `0x600b0000 + {0x00c, 0x040, 0x074}` | `80000000`, `40000000`, `0` | Active/modem/sleep codes 2/1/0. Immediate-update command registers were deliberately not accessed. |
| MAC and FE/baseband gates | SYSCON `0x600a9800 + {0x004, 0x014}` | `01a00000`, `0007e1ff` | ZB APB/MAC, BT/shared baseband, FE 80/160/calibration/APB, and Wi-Fi-named BB clocks are enabled. Wi-Fi MAC/APB bits in CLK_CONF1 are zero. Shared gates do not mean the Wi-Fi/BLE stacks are running. |
| Low-power/control gates | LPCON `0x600af018` | `7` | Wi-Fi-power, coexistence, and analog-I2C clocks enabled; LP timer gate zero. |
| Low-frequency clock source | LPCON `0x600af000 + {0x00c, 0x008, 0x014}` | `f`, `0`, `0` | Four-source divergence described above; coex/32k selectors retained for comparison. |
| RF power-control word | PMU `0x600b0154` | `fc000000` | Captures six upper power/control bits. Their register names do not supply an analog gain or output-power calibration. |
| Channel | MAC `0x600a3048`, bits 0–6 | `17` hex = hop 23 | SDK mapping `(channel - 11) * 5 + 3` gives channel 15, 2425 MHz. |
| TX power-table index | MAC `0x600a304c`, bits 0–4 | `f` = 15 | SDK mapping selects index 15 for a 20 dBm request. This is not a power-meter reading or a PA calibration check. |
| Address/PAN filtering | MAC `0x600a3004`; four banks from `0x600a3008`, stride 16 | Mask 3; programmed bank 0 short `fffe`, PAN `ffff`; bank 1 also enabled | Correct the mask independently of RF gain investigation. Auto-ACK and promiscuous bits are clear. |
| RX break/coexistence controls | MAC `0x600a3000 + {0x004, 0x070}` | No-RSSI-break bit 25 set; force-RX bit 26 clear; coex word `83` | Vendor struct names these as break/arbitration controls. Do not reinterpret bit 25 as an RSSI offset or LNA gain setting. |
| CCA/energy detection | MAC `0x600a3000 + {0x050, 0x054}` | Duration word `07000008`; ED words above | Our transmit path requests CCA. Its acceptance decision is separate from successfully decoded frame RSSI. |
| RF turnaround | MAC `0x600a3100`–`0x600a3118` | PA-on 18, TX-on 35, TXEN-stop 19, TX-off 0, RX-on 50, switch 127, continuous-RX 2 | Values supplied by the linked binary initializer; preserve their pairing. Raw values are not assigned time units without a verified clock/scaling definition. |
| Baseband TX-on | `bt_bb_get_zb_tx_on_delay()` | Existing fixture reads 29 | Vendor binary getter, not a documented public MMIO field. The old override to 50 was already removed. |
| SFD/path timing and BB clock | MAC `0x600a3000 + {0x0c4, 0x0c8, 0x0cc}` | `b`, `001f001f`, `f` | SFD count/path-delay/clock words available for comparison; their physical calibration is not established by these numbers alone. |
| RX DMA metadata | MAC `0x600a30e4`, bits 24–25 | `01010002`: LQI append 1, frequency-offset append 0 | Essential when checking receive-trailer interpretation. RX/TX DMA error words were zero. |
| Interrupt visibility | MAC `0x600a3000 + {0x060, 0x068, 0x078}` | `1cff`, `28000`, `01868000` | Ordinary RX-abort mask enables only ACK-TX timeout/coex aborts. Most CRC/SFD/filter failures therefore do not increment the fixture's IRQ-abort counter. |
| Physical/MAC error evidence | MAC counters `0x600a3144`–`0x600a317c` | Captured CRC, SFD timeout, no-RSS, filter, coex, restart and CCA counters | No clear writes were made. Counts include prior activity and are not attributed to peer frames or this capture window. |
| Antenna routing | [PHY antenna API][antenna], IO MUX/GPIO matrix | No board-specific switch route has been established; [GPIO3/GPIO14 LOW trial](RF_RESULTS.md#gpio3gpio14-antenna-switch-trial--2026-10-06) produced no consistent RSSI gain | There is no generic “PCB antenna” register. External switch controls must correspond to real board wiring. |
| PA/LNA/AGC/synthesizer calibration | `register_chipv7_phy()`, PHY initialization data, `libphy`/`libbtbb` | Vendor-library-owned; public manual does not expose a complete tuning map | Keep input-blob versions, factory trims, calibration mode/result, and loaded-library provenance together. Do not invent field meanings or transplant bytes between library versions. |

The pinned [C6 errata summary][errata] was checked against revision v0.2. It does
not list a matching weak-802.15.4-RSSI erratum. The applicable slow-RC oscillator
item makes clock-source selection worth understanding; it is not evidence that
that erratum has occurred on these boards.

## RSSI provenance

The current [C6 receive path](../../src/port/esp32c6/radio/mod.rs) reads the
signed byte at `raw[length - 1]`. Espressif's
[`ieee802154_rx_frame_info_update`][mac-driver] uses the same byte, takes LQI from
`raw[length]`, and adds the chip-specific compensation. The C6 compensation is
zero. Together with the captured append flags, this supports our current trailer
index and signed interpretation. It does not establish the analog receiver's
gain calibration or absolute RSSI accuracy.

Channel and power-register readbacks agree with the intended request. Remaining
analog uncertainties should be investigated through the exact PHY initialization
and calibration path, not resolved by guessing an RSSI correction constant.

Another latent PAC discrepancy is the arbitration-delay reader's five-bit width
versus the vendor struct/header's eight bits at 16–23. The captured value is 8,
which fits both interpretations, and our port does not write that field through
the narrow accessor. It is a map limitation, not an observed difference here.

## Repeating the snapshot

Build from `experiments/esp32c6-rf`:

```sh
cargo +stable build --release --locked --features rf-registers --bin radio --target-dir target/rf-registers
```

The experiment reuses the kernel radio workload; `rf-registers` adds one snapshot
after the reporter's first one-second wait. The
[register reader](src/radio_registers.rs) contains a bounded list of ordinary
readable locations, emits raw addresses/words, and performs no register writes.
It skips keys, commands, clear registers, and analog-I2C transactions. No new
kernel API or resident storage is introduced. Kernel hardware-test images do not
include these diagnostics. The experiment only includes the extra snapshot when
this feature is selected.

Keep the regular probe ELF separate and restore it after capture. Use USB serial
only. Snapshot output is for configuration comparison; the extra reporting is
not a radio-delivery benchmark.

## Corrections and verification

The C6 port now uses the four correct PAN-enable accessors, so every unrequested
bank is disabled while unrelated control bits are preserved. ED sampling rate
and accumulation mode have separate setters at bits 11–12 and bit 13. Startup
explicitly selects one sample/us, matching the vendor MAC-reset default, and
average accumulation. This also removes a stale rate selection left by the old
bug across warm restarts.

The [vendor clock dependency list][clock] assigns the 802.15.4 MAC, shared BT/154
baseband, ETM, and coexistence clocks to 802.15.4; PHY separately owns FE and
analog-I2C clocks. The Wi-Fi-power LP clock is selected for Wi-Fi or a BLE-specific
workaround, neither active in this port. Startup now gates that unused clock and
clears its four source selectors, rather than inventing a replacement source.
The required shared clocks retain their existing setup.

The hardware run is recorded in `target/rf-assay/fixed-20261006T180958Z/`.
Both boards passed all seven decoded register checks:

| Field | Baseline | Fixed readback on both boards |
| --- | --- | --- |
| CTRL_CFG / enabled PAN mask | `32080000` / 3 | `12080000` / 1 |
| ED rate bits 11–12 | 1 | 0: one sample/us |
| ED mode bit 13 | 0: maximum | 1: average |
| Wi-Fi LP source selectors | `f` | `0` |
| Wi-Fi-power LP gate | 1 | 0 (`LP_CLK = 6`; coex/I2C stay enabled) |
| Channel/hop | 15 / 23 | unchanged |
| Requested TX power/index | 20 dBm / 15 | unchanged |

The one-minute diagnostic run still had missing round trips:

| Final reported statistics | A: `9c:cc:01:40:67:98` | B: `e8:f6:0a:fb:ef:e0` |
| --- | --- | --- |
| Tick | 59,188 | 59,184 |
| Probes / matching replies | 26 / 23 | 27 / 23 |
| Unanswered after peer contact | 2 | 4 |
| Pending probe | 26 | none |
| Pre-peer probes | 0 | 1 |
| API errors / evictions / unmatched replies | 0 / 0 / 0 | 0 / 0 / 0 |
| Sampled last-RSSI range | -83 to -71 dBm | -84 to -71 dBm |

The earlier eight-second assay sampled -76 to -73 dBm. These observations do not
show a signal-strength improvement or establish the cause of the difference.
The configuration defects are corrected; the weak signal and intermittent
unanswered round trips remain unresolved. The final pending probe is not counted
as a failed round trip.

The corrected snapshot, regular probe, and ordinary kernel images compile.
The ordinary and regular-probe ELFs exclude the register dump. The regular probe
with these fixes is the image left on the boards; the snapshot is test-only.
Its subsequent 12-second startup check confirmed peer reception/replies on both
boards, but also one closed unanswered probe each (plus one pending on A).
Sampled RSSI was -85 to -82 dBm on A and -81 to -80 dBm on B. The
`regular-A.log` / `regular-B.log` captures are retained beside the one-minute run.

Further RF investigation should compare matched initialization stages before and
after calibration and MAC setup, retaining 20 dBm. Keep packet-filter/CCA effects
separate from analog signal-strength effects; these fixes do not explain the
entire RSSI problem.

The subsequent [OpenThread control](openthread/README.md) provides working
reference firmware, live register comparisons, and flash/calibration captures.
It also exposed an omitted modem ETM clock enable, now explicit in our port.
The radio fixture now matches the reference CPU setting of 160 MHz. The
nanoESP32-C6 is a visual reference only; the actual board/module identity is
unconfirmed despite the verified C6 silicon and 8 MiB flash.
Its 40-second follow-up answered 17/17 and 18/18 probes at -66 to -61 dBm;
the immediately preceding 80 MHz run was also successful. Weak RSSI remains.

[datasheet]: https://www.espressif.com/sites/default/files/documentation/esp32-c6_datasheet_en.pdf
[trm]: https://www.espressif.com/sites/default/files/documentation/esp32-c6_technical_reference_manual_en.pdf
[mac-reg]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/soc/esp32c6/register/soc/ieee802154_reg.h
[mac-struct]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/soc/esp32c6/register/soc/ieee802154_struct.h
[mac-hal]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/hal/include/hal/ieee802154_common_ll.h
[c6-hal]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/hal/esp32c6/include/hal/ieee802154_ll.h
[mac-driver]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/ieee802154/driver/esp_ieee802154_dev.c
[clock]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/esp_hw_support/modem_clock.c
[syscon]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/soc/esp32c6/include/modem/modem_syscon_reg.h
[lpcon]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/soc/esp32c6/include/modem/modem_lpcon_reg.h
[phy-init]: https://github.com/espressif/esp-idf/blob/v5.5.1/components/esp_phy/src/phy_init.c
[antenna]: https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32c6/api-guides/phy.html
[errata]: https://docs.espressif.com/projects/esp-chip-errata/en/latest/esp32c6/02-errata-summary/index.html
