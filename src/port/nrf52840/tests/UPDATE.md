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
these physical checks. This local fixture retains manual confirmation; signed
OTA watchdog and automatic-confirmation checks are described below.

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

The OTA layout is revision 3: active `0x30000..0x8B000`, DFU plus swap page
`0x8B000..0xE7000`, journal `0xE7000..0xE9000`, protected configuration
`0xE9000..0xED000`. Earlier fixtures require
reprovisioning with both generated factory DFU packages before OTA. For an OTA
fixture already running, use the 1200-baud/DTR sequence for factory recovery; it
does not implement the older diagnostic `dfu` command.

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

Nordic layout 3 requires `--swap-bytes 372736` for cost measurement and
`--minimum-recovery-seconds 65` for the watchdog runner. The whole active slot is
swapped, regardless of candidate length. Physical interruption during a swap has
host fault-injection coverage but remains an unperformed hardware check.
