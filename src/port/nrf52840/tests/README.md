# nRF52840 board checks

The [image staging and trial-boot check](UPDATE.md) exercises the Nordic update
storage and bootloader through USB, using two small kernel images and no RF probe.

The same page describes the `ota-test` A/B fixture for signed image transfer and
durable resume through the actual peer runtime. It is separate from the local
USB diagnostic command fixture.

For the integrated radio path, use the [radio test](../../../kernel/radio/TESTING.md).
For ordinary kernel peer discovery with passive logging, use the
[discovery check](../../../kernel/peer/TESTING.md).

The radio port adds receive-only energy detection beside the HAL packet driver.
It uses `READY_EDSTART` / `EDEND_DISABLE`, `EDCNT+1` windows of 128 us, and the
[Nordic SDK conversion](https://github.com/nrfconnect/sdk-nrfxlib/blob/main/nrf_802154/driver/src/nrf_802154_rssi.c)
`-92 + EDSAMPLE` dBm, without temperature compensation. Multiplication by four
belongs to conversion to an IEEE energy-level code, not raw EDSAMPLE to dBm.
Disabled-event wakeups share the resident task's interrupt
binding with the HAL. Cancellation disables the radio before another operation.
Channel survey and peer discovery have now run on a XIAO nRF52840 Sense alongside
a C6; this is prior bring-up evidence, not validation of the current wire version.

The Nordic port also restores `CCACTRL.CCACORRTHRES=45` and `CCACORRCNT=5`
after HAL construction. Embassy's whole-register CCA mode write clears those
reset fields, disabling correlator-assisted carrier detection. Test images
assert the hardware readback is `0x052d0001` (CarrierMode with restored fields).
This correction alone does not provide CSMA backoff or reliable publication.

## Native appliance path

`test-appliance` binds the existing portable dimmer fixture to ordinary kernel
startup, without a radio probe or passive observer. Its software-backed states are
power and an unsigned 8-bit level; its transformer accepts levels 0..=200 and
registers both actual fixture changes through the kernel. Use it with
`xiao-bootloader,usb-console` to exercise surface reads, native argument/range
validation, and state reads from the tablet's attached radio. `peer-observer` can
be added for passive kernel-event diagnostics; it supplies no networking behavior.
The fixture now also installs four retained level observations and a signed
newest-minus-previous result before startup. All seven scalar states appear in
the ordinary surface and state-read path. The history starts empty; two level
changes make the difference available. This addition has host/build validation;
the hardware run described below predates retained-state installation.
This verifies native dispatch and registered state, not a physical dimmer output.
The final two-board run consumed the full surface, invoked the native operation,
and read registered changes. A read after closing the diagnostic console also
succeeded. Earlier closed-console and post-reflash reads timed out; console state
alone was not established as their cause. This is functional validation, not RF
range or delivery-reliability validation.

## XIAO nRF52840 Sense: USB upload and diagnostics

The `xiao-bootloader` feature selects `memory-xiao.x`: application flash starts
at `0x27000` and stops before `0xED000`, following
[Seeed's S140 v7 application layout](https://github.com/Seeed-Studio/Adafruit_nRF52_Arduino/blob/master/cores/nRF5/linker/nrf52840_s140_v7.ld).
The existing MBR, SoftDevice image, storage, and bootloader stay in flash.
Lattice does not enable the SoftDevice; it owns the radio and all application RAM.
Reset explicitly sets the application's stack pointer and vector table.

The optional `usb-console` feature adds a USB CDC reporter to the **board fixture**.
It uses Embassy's USB driver and stack, with no Embassy executor or allocator.
The existing low-priority reporter polls USB once per millisecond; the kernel's
radio task and IRQ wakeups are unchanged. A disconnected console does not hold up
the radio. The development USB identity is `c0de:cafe`, with the factory device ID
as serial number. This fixture does not drive the board's LEDs.

From this directory:

```sh
cargo +nightly build -Z build-std=core --release --locked --bin radio \
  --features peer-observer,xiao-bootloader,usb-console
uv venv target/dfu-tools
uv pip install --python target/dfu-tools/bin/python \
  adafruit-nrfutil==0.5.3.post16 'setuptools<81'
python3 package_xiao.py target/thumbv7em-none-eabihf/release/radio target/radio-xiao.zip \
  --nrfutil target/dfu-tools/bin/adafruit-nrfutil
```

`package_xiao.py` uses GNU `objcopy`, validates all emitted HEX addresses and the
initial vectors, and produces an **application-only** serial DFU package. It
rejects the standalone image at address zero. `memory.x` is generated only in the
build output directory: keeping another copy beside the manifest would let the
linker silently prefer it over the selected layout.

Select the XIAO's actual serial device; `/dev/ttyACM0` is only an example:

```sh
target/dfu-tools/bin/adafruit-nrfutil --verbose dfu serial \
  --package target/radio-xiao.zip --port /dev/ttyACM0 \
  --baudrate 115200 --singlebank --touch 1200
```

Seeed's factory application and the current XIAO USB diagnostic image support
the 1200-baud/DTR bootloader request. The latter waits 100 ms with DTR deasserted
before resetting with the bootloader's serial-DFU flag (`GPREGRET=0x4e`). A board
already in DFU (`2886:0045` on the Sense) needs no `--touch`. Watch for a changed
serial device after re-enumeration. No UF2 drive is required for serial DFU.
Firmware without this entry hook needs physical bootloader entry or SWD.

Open the resulting Lattice serial device at 115200 with DTR asserted to capture
`DISCOVERY` and `SURFACE` reports. The observer supplies the same test surface and
uses the same peer runtime as the C6. Reporting and USB support are excluded when
the fixture features are absent. The Nordic receive path is identical in ordinary
and observed builds; the HAL does not provide per-packet RSSI, so this fixture
reports no Nordic RSSI sample. Experimental RSSI shortcuts are kept in the lab.

## Standalone SWD scheduler check

This image runs Lattice's existing Cortex-M4F scheduler through `port-nrf52840`.
The port supplies Nordic clock setup, kernel exception bindings, and interrupt
priority limits. The image has no allocator, ESP dependencies, SoftDevice,
Embassy executor, or Embassy time driver.

From this directory, with `thumbv7em-none-eabihf` installed:

```sh
cargo +stable build --release --locked
probe-rs run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/release/lattice-nrf52840-kernel-smoke
```

Logging uses RTT through an SWD probe, so no board-specific LED or UART pin map
is needed. `memory-standalone.x` describes an image beginning at flash address
zero, with 1 MiB flash and 256 KiB RAM. It does not reserve a preinstalled
bootloader or SoftDevice. USB DFU/UF2 boards need their corresponding memory
layout and image packaging; this default is an SWD test image. The separate
XIAO radio image above preserves the stock bootloader.

Expected output is `START`, then `PASS`, followed by periodic `KERNEL` lines.
`START` alone is not a passing test. Assertions check:

- Automatic scheduler vector binding and the 64 MHz / 1 kHz SysTick reload.
- Nested critical sections and a device ISR waking a blocked higher-priority task.
- Tick preemption between equal-priority tasks that never yield.
- Distinct integer and floating-point register canaries across task switches.

The fixture uses the EGU0/SWI0 interrupt as a software-pended device event at
the port's kernel-callable priority. It does not test radio reception. The HAL
starts the required 32 MHz crystal; no external 32 kHz crystal is assumed.

For environments with Rust sources but without the prebuilt target component:

```sh
cargo +nightly build -Z build-std=core --release --locked
```
