# OpenThread reference control

This is a temporary hardware control for the C6 RF investigation, not a Lattice
networking dependency. All device control uses USB serial. Never scan or
reconfigure the host's active Wi-Fi adapter for this test. TX is fixed at 20 dBm.

## Source and build

The reference is Espressif's supported [OpenThread CLI example][example], built
for `esp32c6` from ESP-IDF **v5.5.4**, commit
`735507283d5b2f9fb363a1901172dbd9e847945d`. Its OpenThread submodule is
`a12ff0d0f54fd41954b45047fcdd08f302731c5f`.

The OpenThread/platform sources are available; the low-level RF libraries remain
Espressif binaries. The running control reports PHY version **343** (`b513b46`)
and libbtbb **c493933**, both dated 2025-11-14. Versions, source changes, ELF,
generated flash images, SDK configuration, and library hashes are retained in
`target/openthread-reference/`.

Bench changes are limited to:

- [Configuration overrides](sdkconfig.bench): native USB console, 8 MiB flash,
  20 dBm ceiling, no automatic Thread startup or board LED control.
- [Diagnostic default power](diagnostic-power20.patch): initialize the diagnostic
  command's TX setting to 20 instead of 0, so entering diag mode does not reset it.
- [Read-only register command](register-command.patch): adds `rfregs` to the
  console. It reads only selected ordinary MMIO controls/status, after packet
  measurements. It changes no radio settings.

To reproduce, copy the pinned SDK's `examples/openthread/ot_cli` to a separate
build directory, append `sdkconfig.bench` to its `sdkconfig.defaults`, apply the
power patch inside the pinned OpenThread submodule and the register-command
patch to the copied application, then run `idf.py set-target esp32c6 build`.
The SDK toolchain used here is `riscv32-esp-elf` 14.2.0_20260121; CMake is 3.30.2.
No MAC, PHY, calibration, or radio-port algorithm was modified.

Back up the affected flash range first. This example's partitions end at
`0x150000`; the run saved `[0, 0x150000)` from each board and restored that range
afterward. No writes were made above it. Flash the SDK-generated binaries
verbatim at their specified offsets, preserving the bootloader header. The
earlier `espflash` bootloader-header incompatibility is documented in
[the RF investigation history](../RF_RESULTS.md).

## Validation — 2026-10-06

Board A: `9c:cc:01:40:67:98`. Board B: `e8:f6:0a:fb:ef:e0`.
Data: `target/openthread-reference/20261006T184842Z/`.

Both devices confirmed channel 15 and 20 dBm. Diagnostics used the same broadcast
MAC/payload layout as the Lattice probe, explicit per-frame 20 dBm, no requested
MAC ACK, no retries, and no CCA. Each peer reception was matched to its exact
payload/token. Host pacing differs from the autonomous Lattice round-trip test.

| Direction | Matched frames / sent | RSSI minimum / median / maximum |
| --- | --- | --- |
| A → B | 25 / 25 | -68 / -66 / -61 dBm |
| B → A | 25 / 25 | -68 / -63 / -61 dBm |

Normal Thread operation was then started with a new private bench dataset, still
on channel 15 and at 20 dBm. ICMP ping completed **5/5 each way**. Both devices'
MAC counters reported zero TX retries, CCA errors, and aborts. The early state
queries caught a transient leader/leader phase; subsequent logs show B as a
child and A as leader. Successful bidirectional pings establish actual peer
communication rather than relying on a state string alone.

These short runs validate a working reference link, not a healthy close-range RF
power level or a range result. Lattice immediately after restoration also
answered 6/6 probes each way, at -66 to -61 dBm. The reference did not demonstrate
a unique correction of the RSSI problem. Initialization history and conditions
must be controlled before attributing earlier differences to firmware.

Useful console operations are `ot diag start`, `ot diag channel 15`,
`ot diag power 20`, `ot diag radio receive async 1000 rlp`, `ot diag frame`,
`ot diag send 1`, `ot diag stats`, and `rfregs`. The exact issued commands and
responses are retained in the per-board logs. Diagnostic and ordinary Thread
measurements are kept separate because their MAC behavior differs.

## Captured configuration

Live register dumps were captured in both diagnostic and normal Thread modes,
before any ROM/stub operation that would stop the running firmware. A second
capture (`20261006T185329Z/`) saved each board's reference flash range as well.
The NVS PHY namespace was decoded using the SDK's own NVS tool:

- Per-board calibration format version: **343**.
- Calibration blob: **1,904 bytes** per board, with distinct hashes.
- Factory MAC association retained separately for each calibration.
- Compiled `phy_init_data`: **128 bytes**, extracted from the reference ELF.

The flash reads, decoded NVS, calibration blobs, input block, and their hashes
remain under `target/openthread-reference/`. These capture the parameter surface
needed to reproduce initialization. Per-board calibration and identity remain
associated with their originating board; portable initialization does not imply
copying those bytes between devices or PHY-library versions.

The register comparison is in `20261006T184842Z/register-differences.json`.
Notable static differences from the pre-reference Lattice snapshot:

| Control | Lattice at comparison start | OpenThread control | Interpretation |
| --- | --- | --- | --- |
| CPU divider / clock | 1 / 80 MHz | 0 / 160 MHz | Fixture CPU choice; now aligned below |
| Modem ETM clock | Disabled | Enabled | Present in the SDK's IEEE802154 dependency list; now explicit in our port |
| Wi-Fi-named BB gates | `CLK_CONF1=0x7e1ff` | `0x7e000` | Nine additional BB gates in our extracted setup; investigate against the linked PHY's needs |
| CCA threshold | -60 | -75 | A MAC channel-access choice, not an RSSI correction |
| Coexistence LP clock | `0` | `0x314` | Reference selects the main crystal and divider field 49 |
| Wi-Fi-power LP clock | Off / no source | On / slow RC | SDK system startup selects this under `CONFIG_ESP_WIFI_ENABLED`; does not imply an active Wi-Fi stack |
| Timer event mask | `0x1cff` | `0x1eff` | Reference enables MAC timer 1 overflow |
| PAN/ACK/address fields | Lattice probe | Promiscuous diag or Thread addressing/ACKs | Protocol-specific; not generic RF tuning values |

After the reference, Lattice's raw `MODEM_CLK` read `0x1e00000` rather than the
earlier `0x1a00000`, despite its code not enabling ETM. Its other explicit clock
settings returned to their Lattice values. This exposed inherited hardware
state: the C6 port now enables modem ETM explicitly instead of relying on the
previous firmware. The implementation still needs deliberate control of its
own initialization sequence, not just a copied final register dump.

## nanoESP32-C6 board profile

The [MuseLab maker repository][nano] resembles the described dual USB/native plus
CH343 layout. The user identified it as the closest visual reference, while
explicitly noting that the actual boards lack the WROOM shield marking shown
in its photos and that the Amazon documentation is inaccurate. The actual board
manufacturer/module revision therefore remains unidentified. Direct device
queries establish ESP32-C6 revision v0.2 and 8 MiB flash.

The published MuseLab v1.0 schematic uses an ESP32-C6-WROOM-1 module and shows no
board-level antenna-selection GPIO. That observation applies to the reference
schematic, not automatically to the user's boards. Do not apply clone-specific
pin/antenna assumptions solely from the seller's name or pictures.

The maker's instructions use `idf.py set-target esp32c6`. Its Arduino screenshot
selects **ESP32C6 Dev Module**, CPU **160 MHz**, flash **DIO / 80 MHz**, and USB
CDC on boot. The pictured flash capacity is 16 MiB; our two boards report 8 MiB,
so the reference retained the measured 8 MiB capacity. `nanoESP32-C6` is the board
name, not a different silicon target. Maker sources were pinned at
`ec1ea9f216f65787982682930222ccacfdeb77f2` and retained under
`target/openthread-reference/nanoesp32c6/`.

The C6 radio fixture now uses 160 MHz to match the measured control image;
this choice does not depend on assigning the boards a MuseLab identity.
Together with the explicit ETM enable, a 40-second follow-up answered **17/17 and
18/18** probes, with no pending/unanswered probes, API errors, or evictions at
the final sample. Both read `PCR_CPU_FREQ=0`, `MODEM_CLK=0x1e00000`, and RSSI
between -66 and -61 dBm. Data: `target/openthread-reference/lattice-160mhz/`.
The immediately preceding 80 MHz run was also successful, so this does not prove
that CPU speed caused the earlier missing replies or weak RSSI.

Both boards were left on the updated regular Lattice probe, with no register
dump feature. TX remains 20 dBm. The RF signal-strength question remains open.

[example]: https://github.com/espressif/esp-idf/tree/v5.5.4/examples/openthread/ot_cli
[nano]: https://github.com/wuxx/nanoESP32-C6/tree/ec1ea9f216f65787982682930222ccacfdeb77f2
