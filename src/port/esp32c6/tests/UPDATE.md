# C6 staging and trial-boot check

This fixture starts the normal kernel with an empty appliance and local USB test
commands. It uses the native ESP USB-C port; CH343/UART output alone does not
provide the fixture's input endpoint. These commands are not Lattice OTA messages.
RF remains on the ordinary kernel path with C6 transmit power fixed at 20 dBm.

## Build

Use ESP-IDF **v5.5.4** with its C6 toolchain and Python environment activated, then
run from this directory:

```sh
python update.py prepare
```

`--idf PATH --idf-python PYTHON` can select an installed SDK and its interpreter;
the matching `riscv32-esp-elf` tools must be on PATH. No private lab or appliance
repository is a dependency. The script builds only the vendor bootloader and
partition table, then builds the Rust A/B applications. It checks rollback is
enabled and secure boot, flash encryption, and anti-rollback provisioning are
disabled. It records the SDK revision, config digest, and generated image digests
in `target/update/provenance.json`. Preparation does not access a device.

| Flash region | Purpose |
| --- | --- |
| `0x0000..0x8000` | ESP-IDF second-stage loader, rollback enabled |
| `0x8000..0x9000` | Partition table |
| `0x9000..0xE000` | Reserved NVS; untouched by the update backend |
| `0xE000..0x10000` | Two OTA metadata sectors |
| `0x10000..0x110000` | `ota_0`, 1 MiB |
| `0x110000..0x210000` | `ota_1`, 1 MiB |
| `0x210000..0x212000` | `lattice_update`, 8 KiB transfer journal |

The fixture is packaged for the current 8 MiB C6 development boards. The runtime
reads flash capacity and layout rather than requiring an appliance author to
supply slot addresses. The ROM recovery loader is not flash-resident and remains
available; this provisioning replaces the flash-resident second-stage loader.

## Provision once

Identify the board and save its current flash before changing the layout. Verify
its flash size and that security settings permit this unsigned test image. The
following example uses an already installed `esptool` from the SDK environment:

```sh
python -m esptool --chip esp32c6 --port ACTUAL_PORT read_flash 0 0x800000 target/update/before.bin
python -m esptool --chip esp32c6 --port ACTUAL_PORT write_flash \
  --flash_mode dio --flash_size 8MB --flash_freq 40m \
  0x0 target/update/bootloader.bin \
  0x8000 target/update/partition-table.bin \
  0xe000 target/update/otadata-empty.bin \
  0x10000 target/update/a.bin
```

Only the listed ranges are provisioned; do not erase the entire chip or burn
eFuses. This initial layout installation is distinct from the recoverable
inactive-slot updates being tested. ROM serial download remains the recovery
path if initial provisioning is interrupted.

## Exercise the lifecycle

The Python environment needs `pyserial`. Use the actual native USB serial path:

```sh
python update.py status --port ACTUAL_PORT
python update.py stage --port ACTUAL_PORT target/update/b.bin
python update.py reset --port ACTUAL_PORT
python update.py status --port ACTUAL_PORT
```

Initial A should report `slot=0 boot=Valid`. B's trial should report
`slot=1 boot=PendingVerify`. Reset without confirming: A must return as `Valid`.
Repeat staging B, reset, check B, then run `update.py confirm` and reset again.
B must remain in slot 1 as `Valid`. The loader selects the other image directly;
no copying of a complete slot is needed during reset.

Both port fixtures use the same [host framing helper](../../tests/update_console.py)
and [command decoder](../../tests/update_commands.rs). Commands carry a binary
marker, bounded length, and CRC16; incomplete input expires after 250 ms. Image
writes carry 48 bytes so the entire command fits a 64-byte USB packet. Sequence
correlation distinguishes command results from periodic logs. Build/flash the
fixtures together with their host tools; the older length-only format is retired. The
staging path accepts identical duplicate chunks without programming them again,
and verifies SHA-256 before selecting a trial. This digest check is not release
signature authorization. Explicit reset exercises rollback here; the signed
OTA fixture separately exercises watchdog expiry and automatic confirmation.

## Signed OTA fixture

`LATTICE_UPDATE_PUBLIC_KEY=PUBLIC_KEY_HEX python update.py prepare --ota` builds
A/B images running the common signed receiver and ordinary binary host link in
`target/ota`. Use the same provisioning procedure above with that directory. These
images use `tools/update_peer.py` from the repository root, not the diagnostic
`update.py status/stage` console. See [packaging and transfer](../../../../tools/README.md#packaging-and-transfer).
The supplying peer and receiving peer must both be connected to the formed network.
The receiving USB connection is needed only for explicit local activation
and reset when using unknown costs; healthy signed firmware confirms itself.
Image transfer itself is over radio.

Both OTA fixture tasks use the existing kernel stack-overflow guard. No fixture
keys, generated images, captures, or vendor comparison firmware belong in source.

For OTA provisioning also write `target/ota/journal-empty.bin` at `0x210000`.
The two application slots and OTA selection sectors retain their existing addresses.

## Timing and lifecycle checks

The local USB fixture reports `elapsed_us` for flash operations and `radio_free`
for minimum free radio-task stack. Command 8 verifies without selecting. The
[root tool guide](../../../../tools/README.md#measuring-a-release) owns coefficient
measurement; it uses the local fixture, not the OTA host link.

The common [update check guide](../../TESTING.md#update-checks) owns the maintained
`--activation-check`, `--watchdog-check`, and `--staging-check` recipes and their
validation limits. Build these options with this port's `prepare --ota`, then use
the provisioning procedure above. The separate local `update` image deliberately
retains explicit confirmation so it can exercise both boot outcomes.

C6 selects slots in place: omit `--swap-bytes` and use the watchdog runner's
default 25-second minimum. The [HAL watchdog handoff gap](../../../../docs/FIRMWARE_UPDATES.md#watchdog-backed-startup-confirmation)
remains uncovered. Physical interruption during a flash write/metadata commit is
not established by the host fault-injection checks.

## Installed-program check

Add `--appliance` to `prepare --ota` to bind the existing native dimmer fixture
(power, u8 level, SetLevel and retained level observations) to the binary USB peer.
The release policy is fingerprinted from that actual surface. No automation is
compiled into this image: definitions and enablement arrive after boot through
ordinary program messages. Use `lattice-tools` for installation and inspection.
The fixture exercises MCU dispatch and registered state, not a physical lamp.
