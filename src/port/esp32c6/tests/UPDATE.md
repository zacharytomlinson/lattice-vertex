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
signature authorization. Explicit reset exercises rollback; watchdog-driven
expiry is still to be implemented.

## Validation

On 2026-10-17, an ESP32-C6 revision 0.2 with 8 MiB flash, base MAC ending `67:98`,
completed provisioning and the following sequence through the ordinary kernel.
Its full 8 MiB flash was backed up before provisioning. Secure boot and flash
encryption were disabled; no eFuses were changed.

| Action | Observed image, slot, and state |
| --- | --- |
| Initial startup | A, slot 0, `Valid` |
| Stage B, verify, select and reset | B, slot 1, `PendingVerify` |
| Reset without confirmation | A, slot 0, `Valid` |
| Stage B again, select and reset | B, slot 1, `PendingVerify` |
| Confirm B and reset | B, slot 1, `Valid` |

The candidate image was 190,448 bytes. Reset-acknowledgement to the first successful
USB status took about 0.28 seconds for both trial and rollback, and 0.26 seconds
after confirmation. These observations include an intentional 250 ms host wait
and serial reconnection, so they bound the observed recovery rather than measure
bare bootloader time. The C6 selected the target slot directly without copying it.

On 2026-10-18, an unplug/reconnect with only 52 bytes of an unselected candidate
staged returned to confirmed B in slot 1 (`Valid`, `progress=0`), with kernel
reports running. The incomplete download did not replace the confirmed image.

That check also exposed the old diagnostic parser consuming ASCII `image` as a
command sequence and waiting for a bogus command body. Serial echo/probing is the
suspected source of the text; it was not traced to a particular host service.
The shared marker/checksum decoder fixes that framing failure. On C6, injected log
text was ignored, a corrupt reset command was rejected without rebooting, and a
partial frame expired before the next valid command. The revised 190,928-byte
fixture was staged and confirmed through the running updater. The same decoder
subsequently passed these checks on [Nordic hardware](../../nrf52840/tests/UPDATE.md#validation).
Portable decoder checks run with:

```sh
rustc --edition 2021 --test ../../tests/update_commands.rs -o target/update-command-tests
target/update-command-tests
```

Physical power removal during an individual flash write or metadata commit remains
untested; uncertain metadata writes currently have host fault-injection checks.
Watchdog-driven expiry and OTA transport are separate from this local USB check.

## Signed OTA fixture

`LATTICE_UPDATE_PUBLIC_KEY=PUBLIC_KEY_HEX python update.py prepare --ota` builds
A/B images running the common signed receiver and ordinary binary host link in
`target/ota`. Use the same provisioning procedure above with that directory. These
images use `tools/update_peer.py` from the repository root, not the diagnostic
`update.py status/stage` console. See [signed transfer and packaging](../../../../docs/FIRMWARE_UPDATES.md#packaging-and-maintained-checks).
The supplying peer and receiving peer must both be connected to the formed network.
The receiving USB connection is needed only for explicit local activation,
confirmation, and reset in this check; image transfer itself is over radio.

Both OTA fixture tasks use the existing kernel stack-overflow guard. No fixture
keys, generated images, captures, or vendor comparison firmware belong in source.

For OTA provisioning also write `target/ota/journal-empty.bin` at `0x210000`.
The two application slots and OTA selection sectors retain their existing addresses.

On 2026-10-18, the signed OTA fixture received a 286,208-byte release-2 image
from the Nordic peer over Lattice radio. An interrupted transfer accepted 9,024
bytes but checkpointed 8,192. A local reset recovered 8,192; re-offering the same
package resumed there, completed verification, and left activation explicit. The
remaining transfer plus verification took about 93.5 seconds, excluding erase
preparation. Local selection/reset booted release 2; confirmation followed by
another reset retained release 2. See the [cross-port validation](../../../../docs/FIRMWARE_UPDATES.md#signed-ota-validation-2026-10-18) for limits.


### Timing and automatic activation

The local fixture now reports `elapsed_us` for flash operations and `radio_free`
for the minimum free radio-task stack. Command 8 verifies a candidate without
selecting it. Run `tools/update_costs.py` from the root against these local fixtures
to produce measured package coefficients; it stages, selects, boots and confirms
its candidate. Nordic additionally uses `--swap-bytes 380928` for layout revision 2.

Build `prepare --ota --activation-check` for the signed check. Image A holds
activation for ten seconds after verification through the ordinary public hold
API; image B is the normal OTA receiver. Sign B with the measured cost JSON.
After upload, require A to remain through the hold and quiet interval, then B to
start without an `activate` request. Confirm B locally and reset to verify retention.
Unknown-cost packages still require explicit maintenance. This tests local
activation, not watchdog expiry, a real appliance cycle, or a loss-free absence
exchange. See [the common policy](../../../../docs/FIRMWARE_UPDATES.md#local-activation-policy).


On 2026-10-18, the measured-cost automatic check received 290,144 bytes from the
Nordic. It remained ready on A through the ten-second physical-work hold and
subsequent quiet/notice interval, then booted B without an activation command.
Local confirmation and reset retained B. The guarded local timing fixture also
reported 3,820 bytes of minimum free radio stack after formation with the new
12 KiB ordinary reservation; OTA retains its 24 KiB reservation.


### Watchdog trial check

`prepare --ota --watchdog-check` adds `stall-entry.bin` and `stall-radio.bin` to
healthy A/B. Provision this build's loader and A, sign the three release-2
candidates with explicit unknown costs, then run the shared
[`update_watchdog.py`](../../tests/update_watchdog.py) runner. Nordic needs
`--minimum-recovery-seconds 65`; C6 uses its default 25 seconds. The
[common instructions](../../../../docs/FIRMWARE_UPDATES.md#maintained-trial-failures)
define package filenames and the expected lifecycle.

The stalled candidates mask interrupts and spin forever without a software reset.
The runner waits for hardware expiry and loader recovery, then verifies healthy B
survives a reset without issuing Confirm. Normal OTA builds now confirm local
startup automatically. The separate local USB `update` fixture retains explicit
confirmation for its existing boot-state and timing checks.

On 2026-10-18, both stalled candidates reverted to release 1 without host reset
or confirmation while stalled. Observed return times were 39.41 s at entry
and 35.71 s in the peer task. Healthy B returned in 11.64 s, confirmed itself,
and remained release 2 after another reset with no Confirm command. Times include
USB/host waits and formation, plus Nordic's swaps where applicable. See the
[watchdog validation](../../../../docs/FIRMWARE_UPDATES.md#watchdog-validation-2026-10-18)
for scope and the C6 HAL handoff limitation.

### Background staging check

Build `prepare --ota --staging-check`, provision A as above, and sign B with the
measured cost file. Run the shared
[`update_staging.py`](../../tests/update_staging.py) with `--port` and `--package`.
It checks frozen staging and responsive status during a critical-work hold, then
incremental verification, selection and retained B without host confirmation.
The [maintained validation](../../../../docs/FIRMWARE_UPDATES.md#staging-validation-2026-10-18)
passed on this port with a 292,208-byte image in 324 verification steps. This is a
local receiver/flash check; RF delivery and physical appliance timing are separate.
