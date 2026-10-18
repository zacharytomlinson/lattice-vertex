# C6 image staging and boot metadata

`lattice-c6-boot` connects common inactive-image storage to ESP-IDF's two OTA
selection records. It contains no vendor RTOS, radio, or bootloader copy. The
[port entry](../update.rs) reads the installed partition table, validates the two
OTA slots and metadata reservation, and identifies the running slot through the
MMU mapping. Flash encryption is not supported by this first backend.

The common [storage crate](../../../kernel/update/storage/README.md) owns chunk
writes and SHA-256 checking. This port checks the ESP image header and chip target,
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

Current validation: host checks cover reference CRCs, aborted/torn metadata,
running-slot mismatch, protected rollback images, confirmation, uncertain metadata
commits, bounds, duplicates, and wrong-target images. C6 builds and the maintained
vendor bootloader build pass. Physical validation is pending in the
[USB fixture](../tests/UPDATE.md).

The API is local boot machinery. It is not yet a remotely accessible updater:
package signatures, download resume, watchdog expiry, and autonomous activation
remain common-kernel work. Flash operations are synchronous and can interrupt
radio service; background scheduling is not established by these checks.

```sh
cargo +stable test --lib --locked
```
