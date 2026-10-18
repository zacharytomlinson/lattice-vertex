# Nordic image staging and boot recovery

`lattice-nrf52840-boot` contains the flash boundaries and staging operations shared
by the Nordic kernel port and its separately linked loader. The loader uses
`embassy-boot` 0.7.0 for swap progress, trial boots, and reversion. It contains no
radio, USB stack, task scheduler, or allocator. The kernel consumes the storage
side through `port::nrf52840::update` with `nrf52840-update` enabled.
Chunk staging, padding, duplicate handling, and SHA-256 verification now use the
same [common storage crate](../../../kernel/update/storage/README.md) as C6.
Nordic's flash layout and swap/trial/confirmation state remain in this port.

Current status: host checks, ARM builds, and the XIAO USB staging/trial/confirm/revert
sequence pass. This is local boot/storage integration. Signed packages, download resume,
OTA messages, watchdog-driven trial expiry, and automatic activation are later
common-kernel work. Calling `select_trial` checks SHA-256 and initial vectors; it
does **not** authenticate a release signature.

## XIAO layout

The single definition in [layout.rs](src/layout.rs) is consumed by the loader,
application linker generator, and provisioning tool.

| Range | Purpose |
| --- | --- |
| Below `0x27000` | Existing MBR/SoftDevice, untouched |
| `0x27000..0x2F000` | Lattice loader, 32 KiB reserved |
| `0x2F000..0x30000` | Embassy boot progress, 4 KiB |
| `0x30000..0x8E000` | Active image, 376 KiB |
| `0x8E000..0xED000` | Candidate/rollback image plus swap page, 380 KiB |
| `0xED000` and above | Existing storage/factory recovery loader/settings, untouched |

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

`select_trial` requires the complete image, a matching digest, and vectors linked
for the active slot. Once boot selection starts, further staging and confirmation
are refused in that execution, even if metadata writing returns an error. The
next reset resolves the persistent boot state. An unconfirmed trial cannot erase
its rollback image. `confirm` records successful local startup; a subsequent reset
without confirmation instead reverts through the loader.

Page erase and verification are synchronous. They are not yet suitable for
uninterrupted background radio service. The first maintained check uses explicit
USB commands and resets. A hung application does not yet trigger rollback without
a reset; watchdog integration remains to be added.

Host checks use the real Embassy swap algorithm with a NOR model: bounds and
vectors, duplicate writes, confirmation/reversion, and interruptions during
staging, swapping, and reverting. They are not hardware power-loss evidence.

On the checked XIAO, reset-to-USB-return took approximately 24.6 seconds for a
trial swap and 24.8 seconds for reversion. Confirmation followed by reset returned
within the one-second observation interval. The loader swaps the entire 376 KiB
active slot, even for the 90,684-byte test image. These observations include factory
boot and USB enumeration; they are not guaranteed activation bounds or isolated
flash timings. The release loader occupies 2,804 flash bytes; its swap buffer uses
4 KiB of startup stack. The kernel fixture uses about 23 KiB of static RAM, plus
its startup stack, without an allocator.

```sh
cargo +stable test --lib --locked
cargo +nightly build -Z build-std=core --release --locked \
  --target thumbv7em-none-eabihf --features loader --bin lattice-nrf52840-loader
```

The maintained USB/RTOS fixture and upload instructions are
[beside the port tests](../tests/UPDATE.md).
