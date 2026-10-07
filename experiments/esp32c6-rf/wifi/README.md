# C6 Wi-Fi RF comparison

Standalone diagnostic firmware for comparing the same boards' Wi-Fi and
802.15.4 behavior. It does not depend on or modify the Lattice kernel. The vendor
Wi-Fi stack's allocator and scheduler belong only to this test image.

The access-point image advertises on 2.4 GHz channel 4 (2427 MHz), close to the
802.15.4 test's channel 15 (2425 MHz). The receiver image performs passive scans.
Both use a 20 dBm setting, verified through the Wi-Fi API as 80 quarter-dBm units.
Bluetooth and 802.15.4 are not started. Power saving is disabled. The receiver
first surveys nearby networks, reporting only their count and strongest RSSI;
subsequent records include only this test's `lattice-rf-` networks.

Build from this directory:

```sh
ESP_LOG=info cargo +stable build --release --locked
cp target/riscv32imac-unknown-none-elf/release/lattice-c6-wifi-comparison target/wifi-ap.elf
ESP_LOG=info cargo +stable build --release --locked --features receiver
cp target/riscv32imac-unknown-none-elf/release/lattice-c6-wifi-comparison target/wifi-receiver.elf
python3 run.py /dev/ttyACM1 /dev/ttyACM0
```

The first port receives and the second advertises; `--reverse` exchanges roles.
The runner resolves stable USB identities, records firmware hashes and serial
logs under `target/wifi-comparison/`, then restores the existing Lattice probe
image on each touched board. Close serial monitors before running it.

**Use USB serial only. Do not scan or reconfigure the laptop's Wi-Fi adapter.**
An earlier host scan interrupted the active internet connection; that runner has
been retired. `run.py` has no host wireless commands or network sockets.

Current limitation: these Rust Wi-Fi images initialize and complete scans, but
have not detected the peer or surrounding networks. Zero samples are an
inconclusive control, not a measured weak RSSI or an antenna diagnosis. Driver
logs and the paired combo-PHY/RX initialization calls did not resolve this.
See [the RF investigation](../RF_RESULTS.md) for the reference comparisons.
