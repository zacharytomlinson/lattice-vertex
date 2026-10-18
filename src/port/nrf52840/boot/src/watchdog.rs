//! One watchdog configuration shared by the Nordic loader and application port.
//! Hardware keeps counting during CPU sleep/halt; no interrupt handler feeds it.
use embassy_nrf::pac;
use pac::wdt::{
    regs::Rren,
    vals::{Halt, Rr, Sleep},
};

pub const TIMEOUT_SECONDS: u32 = 30;
const TICKS: u32 = TIMEOUT_SECONDS * 32768 - 1;

pub fn running() -> bool {
    pac::WDT.runstatus().read().runstatus()
}

pub fn start() {
    let wdt = pac::WDT;
    if running() {
        assert_eq!(
            wdt.crv().read(),
            TICKS,
            "unexpected inherited watchdog timeout"
        );
        assert_eq!(
            wdt.rren().read().0,
            1,
            "unexpected inherited watchdog reload channels"
        );
        assert_eq!(wdt.config().read().sleep(), Sleep::Run);
        assert_eq!(wdt.config().read().halt(), Halt::Run);
    } else {
        wdt.config().write(|w| {
            w.set_sleep(Sleep::Run);
            w.set_halt(Halt::Run);
        });
        wdt.crv().write_value(TICKS);
        wdt.rren().write_value(Rren(1));
        wdt.intenclr().write(|w| w.set_timeout(true));
        wdt.tasks_start().write_value(1);
        feed();
    }
    // Do not reload an inherited deadline until local startup has succeeded.
}

pub fn feed() {
    if running() {
        pac::WDT.rr(0).write(|w| w.set_rr(Rr::Reload));
    }
}
