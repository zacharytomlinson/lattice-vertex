# Nordic image staging and boot recovery

`lattice-nrf52840-boot` contains the flash boundaries and staging operations shared
by the Nordic kernel port and its separately linked loader. The loader uses
`embassy-boot` 0.7.0 for swap progress, trial boots, and reversion. It contains no
radio, USB stack, task scheduler, or allocator. The kernel consumes the storage
side through `port::nrf52840::update` with `nrf52840-update` enabled.
Chunk staging, padding, duplicate handling, and SHA-256 verification now use the
same [common storage crate](../../../kernel/storage/README.md) as C6.
Nordic's flash layout and swap/trial/confirmation state remain in this port.

Current status: host checks, ARM builds, and the XIAO USB staging/trial/confirm/revert
sequence pass. The common `kernel::update` service adds signed packages, a durable
transfer journal, and OTA messages above this local backend. The loader now arms
the trial watchdog before application entry, and the common kernel confirms local
startup and reloads the watchdog during task progress. Automatic quiet/hold/neighbor
deferral also lives in the common update service. Calling `select_trial` requires a matching SHA-256 digest and initial vectors; it
does **not** authenticate a release signature.

## XIAO layout

The single definition in [layout.rs](src/layout.rs) is consumed by the loader,
application linker generator, and provisioning tool.

| Range | Purpose |
| --- | --- |
| Below `0x27000` | Existing MBR/SoftDevice, untouched |
| `0x27000..0x2F000` | Lattice loader, 32 KiB reserved |
| `0x2F000..0x30000` | Embassy boot progress, 4 KiB |
| `0x30000..0x8B000` | Active image, 364 KiB |
| `0x8B000..0xE7000` | Candidate/rollback image plus swap page, 368 KiB |
| `0xE7000..0xE9000` | Signed transfer header and page checkpoints, 8 KiB |
| `0xE9000..0xED000` | Protected local configuration, 16 KiB |
| `0xED000` and above | Existing storage/factory recovery loader/settings, untouched |

This is layout revision 3. Provision its loader and application together using
the two uploads; an ordinary OTA package cannot migrate an earlier layout.
The last 64 bytes of the loader reservation contain an immutable descriptor at
`0x2EFC0`; the kernel port checks it before granting flash access. This prevents
an old loader from swapping through the configuration reservation. Existing live
boot/update evidence predates layout 3; the new boundaries have host/build checks,
not physical provisioning or power-loss validation.

The application is always linked at the active address. During reset, the Lattice
loader performs any pending swap/revert, validates the active vectors, resets the
interrupt handoff, and starts the application. Invalid vectors or boot preparation
failure request the existing factory serial DFU path through `GPREGRET=0x4e`.
The SoftDevice radio stack is never enabled.

Factory serial DFU can record a CRC over its complete uploaded application span.
A combined loader/application upload alone is insufficient: changing the active
image or boot-state page would invalidate that CRC. Provision in two steps:
upload the initial combined image, then upload only the immutable Lattice loader.
The factory's single-bank upload erases the uploaded span; the second upload stops
before the active image. The kernel checks the factory settings before granting
flash access and rejects a CRC span extending into mutable regions. No Lattice
code writes the factory settings page; the factory DFU loader maintains it.
This behavior follows the upstream
[factory validation](https://github.com/adafruit/Adafruit_nRF52_Bootloader/blob/0.6.2/lib/sdk11/components/libraries/bootloader_dfu/bootloader.c#L144)
and [single-bank erase path](https://github.com/adafruit/Adafruit_nRF52_Bootloader/blob/0.6.2/lib/sdk11/components/libraries/bootloader_dfu/dfu_single_bank.c).
Both provisioning uploads and the retained software DFU entry have been exercised
on the XIAO nRF52840 Sense fixture.

## Storage operations

`ImageStorage` exclusively owns the flash driver. `begin` bounds one candidate;
`erase_next_page` erases one inactive page per call; `write` appends bounded chunks
and handles final-word padding. Identical written ranges return without another
NOR write. Out-of-order/conflicting writes fail. A write error invalidates the
in-memory candidate, requiring a fresh erase before retrying uncertain flash.
The active image and loader are never written during staging.

`configuration()` lends a bounded flash region to the common configuration store
without creating another peripheral owner. The view is read-only during an
unconfirmed trial or after selection. Image/journal writes and Embassy swap/revert
never include it. The common store's commit generation is local persistence
metadata, not a distributed program identity or traversal counter.

`verify_step` hashes a bounded number of image bytes and checks the vectors when
the digest completes. `verify_image` is its synchronous convenience wrapper.
`select_trial` requires the complete image, a matching digest, and vectors linked
for the active slot. It reuses a completed digest while the candidate region is
unchanged; resume, erases and new writes invalidate that result. Once boot
selection starts, further staging and confirmation
are refused in that execution, even if metadata writing returns an error. The
next reset resolves the persistent boot state. An unconfirmed trial cannot erase
its rollback image. `confirm` records successful local startup; a subsequent reset
without confirmation instead reverts through the loader.

Each page erase remains synchronous. The common updater paces flash steps and
verifies incrementally, but this does not establish uninterrupted radio service.
The local staging check uses explicit USB commands and resets. Signed OTA builds
now confirm locally healthy startup; stalled trials reset through hardware WDT.

Host checks use the real Embassy swap algorithm with a NOR model: bounds and
vectors, duplicate writes, confirmation/reversion, and interruptions during
staging, swapping, and reverting. They are not hardware power-loss evidence.

Physical staging, trial, confirmation, and reversion have been exercised through
the maintained USB fixture. Swap work covers the whole active slot, not merely
the candidate length. Measure each release using the
[cost tool](../../../../tools/README.md#measuring-a-release); historical fixture
timings are not activation bounds. Detailed captures remain in the external lab.

```sh
cargo +stable test --lib --locked
cargo +nightly build -Z build-std=core --release --locked \
  --target thumbv7em-none-eabihf --features loader --bin lattice-nrf52840-loader
```

The maintained USB/RTOS fixture and upload instructions are
[beside the port tests](../tests/UPDATE.md).

Watchdog handoff and automatic local startup confirmation now use the common
[updater startup path](../../../../docs/FIRMWARE_UPDATES.md#watchdog-backed-startup-confirmation).
Provision the new loader before relying on trial-entry protection. No flash
boundaries change. The port reserves its watchdog, and kernel task progress
reloads it after startup; no tick ISR or appliance callback needs to feed it.
A confirmed image remains confirmed after a later watchdog reset.
