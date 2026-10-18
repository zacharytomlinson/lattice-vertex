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

Both port fixtures use the same [host framing helper](../../tests/update_console.py).
Its sequence correlation distinguishes command results from periodic logs. The
staging path accepts identical duplicate chunks without programming them again,
and verifies SHA-256 before selecting a trial. This digest check is not release
signature authorization. Explicit reset exercises rollback; watchdog-driven
expiry is still to be implemented.

Status: host checks and builds pass. Physical provisioning, USB staging, trial,
rollback, confirmation, and power-loss behavior remain unvalidated on C6.
