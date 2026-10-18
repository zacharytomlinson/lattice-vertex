# Nordic staging and trial-boot check

This check runs the ordinary kernel with an empty appliance and a USB command
fixture. Images A and B differ in their reported identity. The fixture drives the
real port storage API; its commands are not OTA messages or permanent kernel API.
See the [boot layout and implementation](../boot/README.md).

## Build and package

From this directory:

```sh
uv venv target/dfu-tools
uv pip install --python target/dfu-tools/bin/python \
  adafruit-nrfutil==0.5.3.post16 'setuptools<81'
python3 update.py prepare --nrfutil "$PWD/target/dfu-tools/bin/adafruit-nrfutil"
```

This builds the loader and both kernel images, validates all emitted addresses
and initial vectors, and produces `target/update/initial.zip`, `loader.zip`,
`a.bin`, and `b.bin`. It does not access USB or flash a board.

## One-time provisioning

Use an actual XIAO nRF52840 with its existing Seeed/Adafruit S140 v7 serial DFU
loader. Identify its serial path rather than assuming a `/dev/ttyACM` number.
The following replaces the current application and clears the new boot-state
region. MBR, SoftDevice, factory loader, and its reserved storage stay outside the
uploaded span. Provisioning is not an interruption-safe OTA transaction; the factory
loader remains the recovery entry if the upload is interrupted.

1. Upload `initial.zip` with the existing application-only command:

   ```sh
   target/dfu-tools/bin/adafruit-nrfutil dfu serial \
     --package target/update/initial.zip --port /dev/serial/by-id/ACTUAL_DEVICE \
     --baudrate 115200 --singlebank --touch 1200
   ```

   Omit `--touch 1200` if already in factory DFU. If factory CRC covers mutable
   regions, the image returns to factory DFU. Loaders with CRC disabled can start
   A immediately; in that case use `update.py dfu --port ACTUAL_LATTICE_DEVICE`.
2. Identify the re-enumerated factory DFU port and upload **`loader.zip`** using
   the same command without `--touch`. This shortens the factory CRC span to the
   immutable loader while retaining image A. Do not skip this step or overwrite
   the factory settings manually.
3. Open the Lattice USB device through the fixture tool:

   ```sh
   target/dfu-tools/bin/python update.py status --port /dev/serial/by-id/ACTUAL_DEVICE
   ```

   Require `image=A`, `boot=Boot`, `ready=1`, and advancing kernel ticks before
   staging. `ready` is the factory-CRC boundary check, not an OTA availability flag.

The loader and kernel fixture retain software factory-DFU entry. `update.py dfu`
or the existing 1200-baud/DTR sequence enters it. If application startup fails,
the fixture panic handler requests that same recovery entry.

## Exercise the lifecycle

Use the tool with the current Lattice serial path:

```sh
target/dfu-tools/bin/python update.py stage --port /dev/serial/by-id/ACTUAL_DEVICE target/update/b.bin
target/dfu-tools/bin/python update.py reset --port /dev/serial/by-id/ACTUAL_DEVICE
target/dfu-tools/bin/python update.py status --port /dev/serial/by-id/ACTUAL_DEVICE
```

After re-enumeration, require `image=B boot=Swap`. Reset again without confirming:
the loader must restore A and report `boot=Revert`. Then stage B once more, reset,
check B, run `update.py confirm`, and reset again. B must remain active and report
`boot=Boot`. Confirmation is explicit so the test can exercise both outcomes.

The tool checks each command result and verifies SHA-256 before selecting a trial.
It leaves reset as a separate action. All commands carry a sequence so periodic
logs cannot be mistaken for a new completion. The shared
[decoder](../../tests/update_commands.rs) requires a binary marker, bounded length,
and CRC16, and expires incomplete input after 250 ms. Packet segmentation is
handled as a USB byte stream. Erase is one page per command; writes use 48-byte
chunks, keeping each command within 64 USB bytes. Build/flash the fixtures with
their matching host tools; the older length-only framing is retired.
No boot images or protocol commands are embedded in ordinary kernel builds.

Once normal trial/revert works, interrupt one staging operation and one swap with
power removal and verify recovery. Keep host fault-injection results separate from
these physical checks. This fixture currently has no automatic watchdog expiry.

## Validation

On 2026-10-16, the XIAO nRF52840 Sense ending `D64C` completed factory provisioning,
USB staging, and this sequence through the ordinary kernel:

| Action | Observed image and state |
| --- | --- |
| Initial startup | A, `Boot` |
| Stage B, verify, select and reset | B, `Swap` |
| Reset without confirmation | A, `Revert` |
| Stage B again, select and reset | B, `Swap` |
| Confirm B and reset | B, `Boot` |

On 2026-10-17, a physical unplug/reconnect with only 52 bytes of a new candidate
staged, and no trial selected, returned to confirmed B: `boot=Boot`, `ready=1`,
`progress=0`, with the kernel and USB command path running. The incomplete download
did not replace the confirmed image. Download progress is currently volatile;
continuing that candidate requires a new staging operation.

On 2026-10-18, the revised 90,924-byte B fixture was installed through the running
updater and booted as `Swap`. It was confirmed and retained `Boot` after another
reset. The shared marker/checksum decoder ignored injected log text, rejected a
corrupt reset command without rebooting, and recovered after a partial frame
expired. Both Nordic and C6 now have hardware validation of the revised console
format. The earlier lifecycle observations above used the original console format.

The 2,804-byte loader and 90,684-byte candidate fit their checked regions. Swap and
reversion took about 25 seconds including USB re-enumeration; the full slot is
swapped regardless of the smaller candidate length. Software factory-DFU entry
also worked. Physical power removal during an individual flash write or swap has
not been exercised; those interruptions currently have host fault-injection checks.

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

The OTA layout is revision 2: active `0x30000..0x8D000`, DFU plus swap page
`0x8D000..0xEB000`, journal `0xEB000..0xED000`. Old revision-1 fixtures require
reprovisioning with both generated factory DFU packages before OTA. For an OTA
fixture already running, use the 1200-baud/DTR sequence for factory recovery; it
does not implement the older diagnostic `dfu` command.

On 2026-10-18, the signed OTA fixture received a 126,364-byte release-2 image
from the C6 peer over Lattice radio. An interrupted transfer accepted 9,024
bytes but checkpointed 8,192. A local reset recovered 8,192; re-offering the same
package resumed there, completed verification, and left activation explicit. The
remaining transfer plus verification took about 41.8 seconds, excluding erase
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


On 2026-10-18, the measured-cost automatic check received 129,620 bytes from C6.
The 31,579 ms estimated outage caused a longer local quiet interval. A remained
ready through 44.38 seconds after verification; B returned at 70.40 seconds after
automatic selection/reset and its swap. The host sent no activation command.
Local confirmation and reset retained B. The local timing fixture measured about
25.01 seconds from trial reset to peer readiness and reported 3,844 bytes of
minimum free radio stack with the corrected ordinary 12 KiB reservation.


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
or confirmation while stalled. Observed return times were 88.27 s at entry
and 79.32 s in the peer task. Healthy B returned in 25.32 s, confirmed itself,
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
passed on this port with a 132,228-byte image in 214 verification steps. This is a
local receiver/flash check; RF delivery and physical appliance timing are separate.
