// Copyright 2021 esp-rs
// SPDX-License-Identifier: MIT
// Adapted from esp-radio 1.0.0-beta.0; see README.md and LICENSE-MIT.

pub(crate) fn enable_ieee802154(en: bool) {
    esp_hal::peripherals::MODEM_SYSCON::regs()
        .clk_conf()
        .modify(|_, w| {
            // Part of ESP-IDF's IEEE802154_CLOCK_DEPS, alongside the ZB MAC
            // and shared baseband. Do not rely on a previous image enabling it.
            w.clk_etm_en().bit(en);
            w.clk_zb_apb_en().bit(en);
            w.clk_zb_mac_en().bit(en)
        });

    esp_hal::peripherals::MODEM_SYSCON::regs()
        .clk_conf1()
        .modify(|_, w| {
            w.clk_fe_apb_en().bit(en);
            w.clk_fe_cal_160m_en().bit(en);
            w.clk_fe_160m_en().bit(en);
            w.clk_fe_80m_en().bit(en);
            w.clk_bt_apb_en().bit(en);
            w.clk_bt_en().bit(en);
            w.clk_wifibb_160x1_en().bit(en);
            w.clk_wifibb_80x1_en().bit(en);
            w.clk_wifibb_40x1_en().bit(en);
            w.clk_wifibb_80x_en().bit(en);
            w.clk_wifibb_40x_en().bit(en);
            w.clk_wifibb_80m_en().bit(en);
            w.clk_wifibb_44m_en().bit(en);
            w.clk_wifibb_40m_en().bit(en);
            w.clk_wifibb_22m_en().bit(en)
        });

    esp_hal::peripherals::MODEM_LPCON::regs()
        .clk_conf()
        .modify(|_, w| w.clk_coex_en().set_bit());
}

pub(crate) fn init_clocks() {
    unsafe {
        esp_hal::peripherals::PMU::regs()
            .hp_sleep_icg_modem()
            .modify(|_, w| w.hp_sleep_dig_icg_modem_code().bits(0));
        esp_hal::peripherals::PMU::regs()
            .hp_modem_icg_modem()
            .modify(|_, w| w.hp_modem_dig_icg_modem_code().bits(1));
        esp_hal::peripherals::PMU::regs()
            .hp_active_icg_modem()
            .modify(|_, w| w.hp_active_dig_icg_modem_code().bits(2));
        esp_hal::peripherals::PMU::regs()
            .imm_modem_icg()
            .write(|w| w.update_dig_icg_modem_en().set_bit());
        esp_hal::peripherals::PMU::regs()
            .imm_sleep_sysclk()
            .write(|w| w.update_dig_icg_switch().set_bit());

        esp_hal::peripherals::MODEM_SYSCON::regs()
            .clk_conf_power_st()
            .modify(|_, w| {
                w.clk_modem_apb_st_map().bits(6);
                w.clk_modem_peri_st_map().bits(4);
                w.clk_wifi_st_map().bits(6);
                w.clk_bt_st_map().bits(6);
                w.clk_fe_st_map().bits(6);
                w.clk_zb_st_map().bits(6)
            });

        esp_hal::peripherals::MODEM_LPCON::regs()
            .clk_conf_power_st()
            .modify(|_, w| {
                w.clk_lp_apb_st_map().bits(6);
                w.clk_i2c_mst_st_map().bits(6);
                w.clk_coex_st_map().bits(6);
                w.clk_wifipwr_st_map().bits(6)
            });

        // ESP-IDF's IEEE802154/PHY clock dependencies do not include the
        // Wi-Fi-power low-frequency clock. Its source is selected for Wi-Fi
        // (or a BLE-specific workaround), neither of which this port starts.
        // Gate it before clearing the old all-source selection; keep shared
        // FE/baseband, analog-I2C, and coexistence clocks under their own setup.
        esp_hal::peripherals::MODEM_LPCON::regs()
            .clk_conf()
            .modify(|_, w| w.clk_wifipwr_en().clear_bit());

        esp_hal::peripherals::MODEM_LPCON::regs()
            .wifi_lp_clk_conf()
            .modify(|_, w| {
                w.clk_wifipwr_lp_sel_osc_slow().clear_bit();
                w.clk_wifipwr_lp_sel_osc_fast().clear_bit();
                w.clk_wifipwr_lp_sel_xtal32k().clear_bit();
                w.clk_wifipwr_lp_sel_xtal().clear_bit()
            });

        esp_hal::peripherals::MODEM_LPCON::regs()
            .wifi_lp_clk_conf()
            .modify(|_, w| w.clk_wifipwr_lp_div_num().bits(0));
    }
}
