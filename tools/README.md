# Firmware package and transfer tools

These maintenance tools exercise the [signed updater](../docs/FIRMWARE_UPDATES.md).
The reusable desktop peer and drag-and-drop interface live in `lattice-tools`;
these scripts remain useful for packaging, timing and port checks. Run from the
repository root with Python `cryptography` and `pyserial` installed. Keys, images,
packages, cost files and captures belong in ignored output directories.

## Packaging and transfer

Generate a release key and provision only its public part into firmware:

```sh
python tools/update_package.py keygen --key /PRIVATE/PATH/release-key
export LATTICE_UPDATE_PUBLIC_KEY=PUBLIC_KEY_HEX
```

Both [Nordic](../src/port/nrf52840/tests/UPDATE.md#signed-ota-fixture) and
[C6](../src/port/esp32c6/tests/UPDATE.md#signed-ota-fixture) use
`update.py prepare --ota` from their own tests directory. Follow that port's
provisioning procedure with its ignored `tests/target/ota` artifacts. Nordic needs
both factory DFU uploads; C6 also writes `journal-empty.bin` at `0x210000`.
Old Nordic layout-1 fixtures need layout-2 loader/application provisioning together;
an ordinary package cannot migrate that layout.

The empty-appliance checks use family `01010101010101010101010101010101`, layout 2,
release A=1 and B=2. Their surface is thirteen bytes: `LSF`, byte 1, byte 32, then
eight zero bytes. Keep that dump beside generated artifacts. Replace `PORT` below
with `src/port/nrf52840` or `src/port/esp32c6`; platform is respectively 1 or 2.

```sh
python tools/update_package.py pack --image PORT/tests/target/ota/b.bin \
  --surface CURRENT_SURFACE --candidate-surface CANDIDATE_SURFACE \
  --key /PRIVATE/PATH/release-key --output PORT/tests/target/ota/b.lvup \
  --platform PLATFORM --layout 2 --release 2 \
  --appliance 01010101010101010101010101010101 --unmeasured
python tools/update_peer.py --port SOURCE_USB info
python tools/update_peer.py --port SOURCE_USB upload --owner RECEIVER_HEX PACKAGE
```

The computer streams through its attached peer to the addressed radio receiver.
`--stop-after BYTES` stops without selection for reset/resume checks. Transfer
retries retain the same immutable package/offset identity; they never retry a
native mutation. Successful upload means verified staging, not a running release.

`--unmeasured` records unknown timing for explicitly activated checks. After upload,
local maintenance can select/reset and inspect the receiver:

```sh
python tools/update_peer.py --port RECEIVER_USB info
python tools/update_peer.py --port RECEIVER_USB activate
# Reconnect after trial startup; inspect the running release and local operation.
python tools/update_peer.py --port RECEIVER_USB confirm
python tools/update_peer.py --port RECEIVER_USB reset
```

The signed receiver normally confirms its own healthy startup; manual Confirm is
idempotent after those checks and Busy beforehand. These controls do not belong
to the radio protocol. Measured packages can activate autonomously instead.

Release packaging requires `--costs JSON` rather than `--unmeasured`. JSON contains
`erase_page_us`, `write_per_byte`, `verify_per_byte`, `activation_us`,
`swap_per_byte`, and `longest_pause_us`, with the units defined in the updater.
Current/candidate surface dumps must be byte-identical. The signing pipeline must
supply authentic dumps and the matching candidate release; the packager cannot
prove those properties from arbitrary machine code.

For one subsequent candidate without reprovisioning, set `LATTICE_UPDATE_RELEASE=N`
when building its `ota` binary and use the same signed release. Do not apply the
same override to both A/B fixtures: that defeats a version-advancing check.

## Measuring a release

`update_costs.py` uses the local USB `update` fixture, not the binary OTA host link.
It requires a provisioned matching fixture and never installs a layout. It stages
a candidate, measures flash operations, selects/boots it, verifies changed fixture
identity, and confirms it. This modifies the board; build preparation alone does not.

```sh
python tools/update_costs.py --port ACTUAL_PORT --image CANDIDATE_BIN \
  --output costs.json --swap-bytes 380928
```

The swap extent above is Nordic layout 2; omit `--swap-bytes` for C6. Port fixtures
report `elapsed_us` excluding serial output: C6 uses its monotonic timer and Nordic
wrapping differences from its 64 MHz cycle counter. Erase cost uses the largest
observed page time; programming/verification use Q24.8 microseconds per byte.

The tool measures reset-to-peer readiness for original and candidate images. On
Nordic their difference supplies swap cost per byte. Baseline startup and cached
selection form the fixed cost. Verification belongs to staging. The default 125%
margin rounds upward; JSON also records provenance. NetworkSelected avoids treating
USB enumeration alone as peer readiness. USB/host waits make these conservative
planning inputs, not worst-case guarantees. Empty fixtures do not measure real
appliance initialization or work; qualify actual release builds before publication.

## Maintained checks

The common [port check guide](../src/port/TESTING.md#update-checks) owns activation,
watchdog and staging recipes. Port-specific build/upload details stay in each
port's `UPDATE.md`. Physical power interruption during an individual flash write
or swap is distinct from reset/resume and host fault injection.
