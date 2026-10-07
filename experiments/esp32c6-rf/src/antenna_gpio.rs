//! Reversible GPIO3/GPIO14-low trial for the suspected XIAO-style RF switch.
//! These pins are not assumed to control an antenna on the unidentified boards.

use esp_hal::peripherals::{GPIO, IO_MUX};

#[cfg(feature = "antenna-gpio-test")]
const PINS: [usize; 2] = [3, 14];
const MASK: u32 = (1 << 3) | (1 << 14);

pub fn report(stage: &str) {
    let gpio = GPIO::regs();
    esp_println::println!(
        "ANT_GPIO stage={} enable=0x{:x} out=0x{:x} gpio3_mux=0x{:x} gpio14_mux=0x{:x}",
        stage,
        gpio.enable().read().bits() & MASK,
        gpio.out().read().bits() & MASK,
        IO_MUX::regs().gpio(3).read().bits(),
        IO_MUX::regs().gpio(14).read().bits(),
    );
}

#[cfg(feature = "antenna-gpio-test")]
mod trial {
    use super::*;
    use esp_hal::gpio::{Level, Output, OutputConfig};

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct SavedPins {
        output: u32,
        enable: u32,
        mux: [u32; 2],
        pin: [u32; 2],
        selection: [u32; 2],
    }

    impl SavedPins {
        fn read() -> Self {
            let gpio = GPIO::regs();
            Self {
                output: gpio.out().read().bits() & MASK,
                enable: gpio.enable().read().bits() & MASK,
                mux: PINS.map(|pin| IO_MUX::regs().gpio(pin).read().bits()),
                pin: PINS.map(|pin| gpio.pin(pin).read().bits()),
                selection: PINS.map(|pin| gpio.func_out_sel_cfg(pin).read().bits()),
            }
        }
    }

    // Set once before scheduler startup; consumed only by the reporting task.
    static mut SAVED: Option<SavedPins> = None;

    pub fn drive_low(
        enable: esp_hal::peripherals::GPIO3<'static>,
        select: esp_hal::peripherals::GPIO14<'static>,
    ) -> (Output<'static>, Output<'static>) {
        unsafe { SAVED = Some(SavedPins::read()) };
        let enable = Output::new(enable, Level::Low, OutputConfig::default());
        esp_hal::delay::Delay::new().delay_millis(100);
        let select = Output::new(select, Level::Low, OutputConfig::default());
        report("driven-low");
        (enable, select)
    }

    pub fn restore() {
        let saved = unsafe { SAVED.take().expect("antenna GPIO trial not initialized") };
        let gpio = GPIO::regs();
        // Undo exactly the registers touched by Output::new/apply_config.
        // Mask shared GPIO words so unrelated pins retain their current state.
        unsafe {
            gpio.enable_w1tc().write(|w| w.bits(MASK));
            gpio.out_w1tc().write(|w| w.bits(MASK & !saved.output));
            gpio.out_w1ts().write(|w| w.bits(saved.output));
            for (index, pin) in PINS.into_iter().enumerate() {
                gpio.pin(pin).write(|w| w.bits(saved.pin[index]));
                gpio.func_out_sel_cfg(pin)
                    .write(|w| w.bits(saved.selection[index]));
                IO_MUX::regs().gpio(pin).write(|w| w.bits(saved.mux[index]));
            }
            gpio.enable_w1ts().write(|w| w.bits(saved.enable));
        }
        assert!(
            SavedPins::read() == saved,
            "antenna GPIO state was not restored"
        );
        report("restored");
    }
}

#[cfg(feature = "antenna-gpio-test")]
pub use trial::{drive_low, restore};
