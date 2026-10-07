# ESP32-C6 radio source

`mac.rs`, `registers.rs`, `settings.rs`, and `clocks.rs` are adapted from
`esp-radio` 1.0.0-beta.0 in [esp-rs/esp-hal](https://github.com/esp-rs/esp-hal).
The source package records commit `b4c8d9bc634373bc140df1c3c83ba42706a55944`.
The MIT license is retained beside these files.

The extracted code supplies the C6 MAC state machine, registers, parameters,
and required radio clocks. PHY calibration and binary support remain provided
by `esp-phy` and `esp-wifi-sys-esp32c6`; Wi-Fi/BLE stacks and the ESP RTOS are
not started. Enabling shared modem clocks does not start those protocols.

Local changes: select only C6 register/power handling; remove allocating frame
APIs; use an aligned static DMA receive buffer and four fixed receive entries;
evict the oldest frame on overflow and report it to test instrumentation when
enabled; replace arbitrary callbacks
with kernel task wakers. The hardware-test workload sends frames without MAC ACK requests.
One transmission is one attempt, with no application retry.

The linked `esp-wifi-sys-esp32c6` 0.2.0 `libbtbb.a` initializes baseband TX-on
delay to 29, then initializes MAC TX-on/RX-on/TX-RX-switch values to 35/50/127.
This was verified in `bt_bb_v2_init_cmplx`, `bt_bb_set_zb_tx_on_delay`, and
`ieee802154_txon_delay_set` using `riscv64-linux-gnu-objdump -dr`. The older Rust
driver overwrote the baseband value with 50 after initialization. That override
and the redundant RX-on write are removed; preserve the vendor initializer's
settings together instead of hardcoding them again.

The hardware fixture reports the baseband getter and actual MAC timing/channel/
power registers, plus hardware error counters. This checks the programmed values;
it does not measure radiated power. The linked library's TX-power table has 16
entries and its final entry is 20, consistent with the current index-15 maximum.

The C6 port also selects combo-radio PHY initialization before enabling the PHY
and explicitly disables Wi-Fi RX afterward. `esp-phy` 0.2.0 has that selection
commented out; [upstream PR #5776](https://github.com/esp-rs/esp-hal/pull/5776)
restores it together with protocol-specific Wi-Fi RX control. The retained vendor
reference also calls `phy_init_param_set(1)` before initialization. This is
restricted to our 802.15.4-only port; adding concurrent Wi-Fi would need its own
receiver/coexistence lifecycle rather than inheriting this setting.

The register assay also corrected the PAN-enable mask to bits 28:31 and separated
ED sample rate (bits 11:12) from maximum/average mode (bit 13). The PAC's ED field
labels conflict with the operational ESP-IDF C struct, so these two accesses use
explicit masks. Initialization selects one sample/us and average accumulation.
The unused Wi-Fi-power low-frequency clock is gated with all source selectors
cleared; 802.15.4's shared FE/baseband, analog-I2C, and coexistence clocks remain.
Both boards' readbacks confirm the fixes. See the
[assay and hardware results](../../../../experiments/esp32c6-rf/RF_ASSAY.md#corrections-and-verification).

The modem ETM clock is also explicitly enabled with the ZB MAC/APB clocks, matching
the vendor's IEEE802154 dependency list. The OpenThread comparison exposed that
this bit could otherwise be inherited from previously flashed firmware.

Energy detection follows ESP-IDF v5.5.4's
[`ieee802154_energy_detect` and ED interrupt path](https://github.com/espressif/esp-idf/blob/v5.5.4/components/ieee802154/driver/esp_ieee802154_dev.c)
and [common register helpers](https://github.com/espressif/esp-idf/blob/v5.5.4/components/esp_hal_ieee802154/include/hal/ieee802154_common_ll.h).
Its duration is in 16-us symbols. The port uses maximum accumulation during the
scan and restores normal average mode on completion, abort, or cancellation.
ED completion wakes the same resident task as packet I/O; no polling task or
allocator is added. Changing channels stops the old operation and clears queued
RX frames before applying new settings. TX remains fixed at 20 dBm.

Channel surveying/selection is common-kernel policy. The
[formation hardware check](../../../kernel/peer/TESTING.md#formation-validation-status--2026-10-07)
exercised ED and retuning on both C6s, including selection, joining, founder
restart, and simultaneous starts. The earlier register readbacks alone did not
validate those scan operations.
