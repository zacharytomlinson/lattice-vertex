# C6 image staging and boot metadata

`lattice-c6-boot` connects common inactive-image storage to ESP-IDF's two OTA
selection records. It contains no vendor RTOS, radio, or bootloader copy. The
[port entry](../update.rs) reads the installed partition table, validates the two
OTA slots and metadata reservation, and identifies the running slot through the
MMU mapping. Flash encryption is not supported by this first backend.

The common [storage crate](../../../kernel/storage/README.md) owns chunk
writes and incremental SHA-256 checking. `verify_step` bounds image bytes per call;
`verify_image` is its synchronous convenience wrapper. Selection reuses the
completed digest while the candidate region remains unchanged. Resume, erases
and new writes invalidate that result. This port checks the ESP image header and chip target,
then writes a `New` record to the metadata sector opposite the running image's
record. It never assumes that the highest raw sequence identifies the running
image: invalid CRCs, invalid states, and aborted trials are excluded. The port
also compares the selected record with the actual mapping before allowing writes.

Metadata uses the layout and CRC semantics of ESP-IDF 5.5.4's
[OTA selector](https://github.com/espressif/esp-idf/blob/v5.5.4/components/bootloader_support/src/bootloader_common_loader.c).
Records are decoded as bytes, including unrecognized/torn state values. The
convenience `esp-bootloader-esp-idf` 0.5.0 OTA selector is not used because its
sequence-only selection can select an aborted entry after rollback. Its validated
partition reader and running-partition lookup are reused by the hardware port.

The old confirmed record remains intact while a candidate is selected. The new
record's sequence and state are written before its CRC commit word. An uncertain
metadata mutation blocks further staging/confirmation in the current execution;
reset lets the loader resolve persistent state. Confirmation updates only the
running image's record. Sequence exhaustion returns an error instead of wrapping.

With rollback enabled, the vendor loader changes `New` to `PendingVerify` before
entering the candidate. Reset without confirmation marks it `Aborted` and selects
the previous valid image. `confirm` records `Valid`. Seeing a running image still
marked `New` is rejected as missing rollback support. There is no slot-copy outage
as on Nordic; both images stay in their own flash regions.

Host checks cover reference CRCs, aborted/torn metadata, running-slot mismatch,
protected rollback images, confirmation, uncertain commits, bounds, duplicate
writes, and wrong-target images. Physical staging, trial, confirmation and rollback
have been exercised through the [maintained fixtures](../../TESTING.md#update-checks).
Physical interrupted writes remain separate from host fault injection. Historical
capture details and timings are preserved in the external lab.

The API is local boot machinery. The optional `kernel::update` service adds signed
packages, download resume, and radio transfer above it. Its validated partition
table reservation `lattice_update` (data subtype `0x40`) supplies the 8 KiB journal.
Layout 3 adds `lattice_config` (data subtype `0x41`), 16 KiB at `0x212000`.
The port validates its bounds/disjointness and lends it through `configuration()`;
unconfirmed trials and selected updates receive a read-only view. Image selection,
staging and journal writes cannot reach it. Host checks cover the reservation and
committed-copy storage; this layout has not yet been provisioned on hardware.
The common updater now applies measured-cost, local-activity and neighbor deferral
before automatic activation. Startup confirmation and hardware watchdog recovery
are now implemented, with the documented esp-hal initialization handoff gap.
The common updater paces flash steps and verifies incrementally. Individual flash
operations remain synchronous and can interrupt radio service.

```sh
cargo +stable test --lib --locked
```

Watchdog handoff and automatic local startup confirmation now use the common
[updater startup path](../../../../docs/FIRMWARE_UPDATES.md#watchdog-backed-startup-confirmation).
Provision the loader and layout-3 partition table together. OTA image boundaries
remain unchanged; the new configuration partition lies beyond the journal.
The port reserves its watchdog, and kernel task progress
reloads it after startup; no tick ISR or appliance callback needs to feed it.
A confirmed image remains confirmed after a later watchdog reset.
