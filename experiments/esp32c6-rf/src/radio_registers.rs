//! One read-only register snapshot from the running C6 radio fixture.
//! Offsets: ESP-IDF v5.5.1 C6 register headers and TRM v1.2 (see RF_ASSAY.md).
//! Keep raw words: some PAC field names disagree with the vendor's active HAL.

fn bank(base: usize, registers: &[(usize, &str)]) {
    for &(offset, name) in registers {
        // These explicit, aligned MMIO locations are ordinary readable control,
        // status, and counter registers on C6. No command, interrupt-clear,
        // reset strobe, security key, or analog-I2C transaction is accessed.
        let value = unsafe { core::ptr::read_volatile((base + offset) as *const u32) };
        esp_println::println!(
            "REG addr=0x{:08x} name={} value=0x{:08x}",
            base + offset,
            name,
            value
        );
    }
}

pub fn report() {
    esp_println::println!("REG_BEGIN mac={}", esp_hal::efuse::base_mac_address());
    bank(
        0x600a_3000,
        &[
            (0x004, "ZB_CTRL"),
            (0x048, "ZB_CHANNEL"),
            (0x04c, "ZB_TX_POWER"),
            (0x050, "ZB_ED_DURATION"),
            (0x054, "ZB_ED_CFG"),
            (0x058, "ZB_IFS"),
            (0x05c, "ZB_ACK_TIMEOUT"),
            (0x060, "ZB_EVENT_EN"),
            (0x068, "ZB_RX_ABORT_EN"),
            (0x070, "ZB_COEX_PTI"),
            (0x078, "ZB_TX_ABORT_EN"),
            (0x080, "ZB_RX_STATUS"),
            (0x084, "ZB_TX_STATUS"),
            (0x088, "ZB_TXRX_STATUS"),
            (0x090, "ZB_CORE_GCK"),
            (0x0c4, "ZB_SFD_WAIT"),
            (0x0c8, "ZB_PATH_DELAY"),
            (0x0cc, "ZB_BB_CLK"),
            (0x0d4, "ZB_TX_DMA_CFG"),
            (0x0d8, "ZB_TX_DMA_ERR"),
            (0x0e4, "ZB_RX_DMA_CFG"),
            (0x0e8, "ZB_RX_DMA_ERR"),
            (0x0f0, "ZB_DMA_GCK"),
            (0x100, "ZB_PA_ON"),
            (0x104, "ZB_TX_ON"),
            (0x108, "ZB_TXEN_STOP"),
            (0x10c, "ZB_TX_OFF"),
            (0x110, "ZB_RX_ON"),
            (0x114, "ZB_SWITCH"),
            (0x118, "ZB_CONT_RX"),
            (0x11c, "ZB_DCDC"),
            (0x144, "ZB_SFD_TIMEOUTS"),
            (0x148, "ZB_CRC_ERRORS"),
            (0x14c, "ZB_ED_ABORTS"),
            (0x150, "ZB_CCA_FAILURES"),
            (0x154, "ZB_FILTER_FAILURES"),
            (0x158, "ZB_NO_RSS"),
            (0x15c, "ZB_RX_COEX"),
            (0x160, "ZB_RX_RESTARTS"),
            (0x174, "ZB_TX_COEX"),
            (0x17c, "ZB_CCA_BUSY"),
            (0x184, "ZB_DATE"),
        ],
    );
    for interface in 0..4 {
        esp_println::println!("REG_INTERFACE index={}", interface);
        bank(
            0x600a_3008 + interface * 16,
            &[
                (0, "SHORT_ADDR"),
                (4, "PAN_ID"),
                (8, "EXT_ADDR_LO"),
                (12, "EXT_ADDR_HI"),
            ],
        );
    }
    bank(
        0x600a_9800,
        &[
            (0x04, "MODEM_CLK"),
            (0x08, "MODEM_CLK_FORCE"),
            (0x0c, "MODEM_CLK_MAP"),
            (0x14, "MODEM_CLK1"),
            (0x18, "MODEM_CLK1_FORCE"),
            (0x1c, "MODEM_WIFI_BB"),
            (0x20, "MODEM_MEM"),
        ],
    );
    bank(
        0x600a_f000,
        &[
            (0x08, "LP_COEX_CLK"),
            (0x0c, "LP_WIFI_CLK"),
            (0x10, "LP_I2C_CLK"),
            (0x14, "LP_MODEM_32K"),
            (0x18, "LP_CLK"),
            (0x1c, "LP_CLK_FORCE"),
            (0x20, "LP_CLK_MAP"),
            (0x28, "LP_MEM"),
        ],
    );
    bank(
        0x6009_6000,
        &[
            (0x108, "PCR_MODEM_APB"),
            (0x110, "PCR_SYSCLK"),
            (0x118, "PCR_CPU_FREQ"),
            (0x11c, "PCR_AHB_FREQ"),
            (0x120, "PCR_APB_FREQ"),
        ],
    );
    bank(
        0x600b_0000,
        &[
            (0x00c, "PMU_ACTIVE_MODEM_ICG"),
            (0x040, "PMU_MODEM_MODEM_ICG"),
            (0x074, "PMU_SLEEP_MODEM_ICG"),
            (0x154, "PMU_RF_PWR"),
        ],
    );
    bank(
        0x600b_0400,
        &[(0x00, "LP_CLK_CONF"), (0x20, "LP_CLK_TO_HP")],
    );
    esp_println::println!("REG_END");
}
