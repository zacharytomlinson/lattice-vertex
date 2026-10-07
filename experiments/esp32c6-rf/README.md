# Temporary ESP32-C6 RF experiments

These investigate signal strength, PHY configuration, and board wiring. They are
not the kernel's hardware test suite. Keep them temporary: validated fixes belong
in the platform port, and repeatable kernel checks belong beside the port or
kernel module they exercise.
Neither the kernel nor its hardware-test crates depend on this directory.

| Area | Purpose |
| --- | --- |
| Root Rust image | Run the existing kernel radio workload with RF timing/register diagnostics |
| `rf-registers` feature | One raw MMIO snapshot for the [register assay](RF_ASSAY.md) |
| `antenna-gpio-test` feature | Reversible GPIO3/14 LOW experiment; no assumption that the boards have this antenna switch |
| [wifi](wifi/README.md) | Standalone vendor Wi-Fi stack, allocator, and scheduler; no Lattice kernel |
| [openthread](openthread/README.md) | Pinned ESP-IDF/OpenThread control and diagnostic patches |
| [reference.py](reference.py) | USB runner for the retained ESP-IDF IEEE 802.15.4 CLI |
| [RF_RESULTS.md](RF_RESULTS.md) | Investigation history and measured comparisons |

The root experiment reuses `src/kernel/radio/tests` for its packet workload. Its
additional GPIO and register diagnostics live here. The actual kernel radio
checks are indexed in [the port testing guide](../../src/port/TESTING.md).

From this directory, build the baseline or one of the optional trials:

```sh
cargo +stable build --release --locked --bin radio
cargo +stable build --release --locked --features rf-registers --bin radio --target-dir target/rf-registers
cargo +stable build --release --locked --features antenna-gpio-test --bin radio --target-dir target/antenna-gpio
```

The image uses channel 15 and **20 dBm** through the existing RF test workload.
GPIO/register reporting does not belong to ordinary kernel startup or the kernel
test binaries. Use USB serial only; do not scan or reconfigure the laptop Wi-Fi
adapter. Nothing is flashed by a build command.

Vendor and Wi-Fi runners restore the regular kernel radio-probe ELF at
`src/port/esp32c6/tests/target/riscv32imac-unknown-none-elf/release/radio`, relative
to the repository root. Build that image with `--features radio-probe` before
running either comparison. The vendor runner offers `--dry-run` to verify paths
and artifacts without touching the boards.

Existing captures, SDK checkouts, backups, and firmware hashes remain under the
repository's ignored `target/` directories. Historical paths in those captures
describe the run at the time; the source migration does not rewrite that evidence.
