//! Kernel-owned RTC watchdog. Bootloader keeps it across application entry;
//! esp-hal 1.1.2 disables it in init, so port initialization rearms it immediately.
use esp_hal::{
    peripherals::{LPWR, LP_WDT},
    rtc_cntl::{Rtc, RwdtStage},
    time::Duration,
};

pub(crate) const TIMEOUT_SECONDS: u64 = 30;

pub(crate) fn running() -> bool {
    LP_WDT::regs().wdtconfig0().read().wdt_en().bit_is_set()
}

pub(crate) fn start() {
    // Rtc::new only constructs handles. The update port reserves the watchdog;
    // it does not take over the RTC clock or other LPWR functions.
    let mut rtc = Rtc::new(unsafe { LPWR::steal() });
    rtc.rwdt
        .set_timeout(RwdtStage::Stage0, Duration::from_secs(TIMEOUT_SECONDS));
    rtc.rwdt.enable();
    rtc.rwdt.unlisten();
    feed();
}

pub(crate) fn feed() {
    if running() {
        let mut rtc = Rtc::new(unsafe { LPWR::steal() });
        rtc.rwdt.feed();
    }
}
