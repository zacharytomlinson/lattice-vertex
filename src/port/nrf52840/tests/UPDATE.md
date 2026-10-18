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
logs cannot be mistaken for a new completion. Packet segmentation is handled as
a USB byte stream. Erase is one page per command; writes use 52-byte chunks.
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

The 2,804-byte loader and 90,684-byte candidate fit their checked regions. Swap and
reversion took about 25 seconds including USB re-enumeration; the full slot is
swapped regardless of the smaller candidate length. Software factory-DFU entry
also worked. Physical power removal during an individual flash write or swap has
not been exercised; those interruptions currently have host fault-injection checks.
