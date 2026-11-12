//! Kernel-owned GPIOTE channels. Edge qualification is peripheral work; the ISR
//! acknowledges the event, performs a bounded action and returns. No input mirror.
use super::{hal, KERNEL_INTERRUPT_PRIORITY};
use crate::kernel::{state, Edge, Interrupt, Pull};
use hal::pac::interrupt;
use hal::{
    gpio::{Input, Port},
    interrupt::InterruptExt,
    pac::gpiote::vals::{Mode, Polarity},
};
#[derive(Clone, Copy)]
struct Handler {
    action: fn(),
    enabled: bool,
}
static mut HANDLERS: [Option<Handler>; 8] = [None; 8];
static mut STARTED: bool = false;
/// Consume an input and reserve one GPIOTE channel before kernel startup.
/// `action` executes in the IRQ, must be bounded/nonblocking, and can use State
/// writes and kernel timers/deferred work. The kernel acknowledges the event.
/// Inputs are armed by kernel startup, after appliance initialization.
/// This port owns GPIOTE; do not enable embassy-nrf's separate gpiote driver.
pub fn on_edge(
    pin: hal::Peri<'static, impl hal::gpio::Pin>,
    pull: Pull,
    edge: Edge,
    action: fn(),
) -> Interrupt {
    state::atomic(|| unsafe {
        assert!(!STARTED, "declare inputs before kernel startup");
        let handlers = &mut *core::ptr::addr_of_mut!(HANDLERS);
        let i = handlers
            .iter()
            .position(Option::is_none)
            .expect("GPIOTE channels exhausted");
        let number = pin.pin();
        let port = pin.port();
        core::mem::forget(Input::new(
            pin,
            match pull {
                Pull::None => hal::gpio::Pull::None,
                Pull::Up => hal::gpio::Pull::Up,
                Pull::Down => hal::gpio::Pull::Down,
            },
        ));
        hal::pac::GPIOTE.intenclr(0).write(|w| w.0 = 1 << i);
        hal::pac::GPIOTE.config(i).write(|w| {
            w.set_mode(Mode::Event);
            w.set_psel(number);
            w.set_port(matches!(port, Port::Port1));
            w.set_polarity(match edge {
                Edge::Rising => Polarity::LoToHi,
                Edge::Falling => Polarity::HiToLo,
                Edge::Either => Polarity::Toggle,
            });
        });
        hal::pac::GPIOTE.events_in(i).write_value(0);
        handlers[i] = Some(Handler {
            action,
            enabled: true,
        });
        Interrupt::new(i as u8, set, read)
    })
}
fn read(index: u8) -> bool {
    state::atomic(|| {
        let config = hal::pac::GPIOTE.config(index as usize).read();
        let port = if config.port() {
            hal::pac::P1
        } else {
            hal::pac::P0
        };
        port.in_().read().0 & (1 << config.psel()) != 0
    })
}
fn set(index: u8, enabled: bool) {
    state::atomic(|| unsafe {
        let i = index as usize;
        let h = (&mut *core::ptr::addr_of_mut!(HANDLERS))[i]
            .as_mut()
            .unwrap();
        h.enabled = enabled;
        hal::pac::GPIOTE.intenclr(0).write(|w| w.0 = 1 << i);
        hal::pac::GPIOTE.events_in(i).write_value(0);
        if enabled && STARTED {
            hal::pac::GPIOTE.intenset(0).write(|w| w.0 = 1 << i);
        }
    });
}
pub(crate) fn start() {
    state::atomic(|| unsafe {
        STARTED = true;
        for i in 0..8 {
            if let Some(h) = HANDLERS[i] {
                set(i as u8, h.enabled);
            }
        }
        hal::interrupt::GPIOTE.set_priority(KERNEL_INTERRUPT_PRIORITY);
        hal::interrupt::GPIOTE.unpend();
        hal::interrupt::GPIOTE.enable();
    });
}
#[cortex_m_rt::interrupt]
fn GPIOTE() {
    for i in 0..8 {
        let action = state::atomic(|| unsafe {
            let h = HANDLERS[i]?;
            if !h.enabled || hal::pac::GPIOTE.events_in(i).read() == 0 {
                return None;
            }
            hal::pac::GPIOTE.events_in(i).write_value(0);
            Some(h.action)
        });
        if let Some(action) = action {
            state::atomic(action);
        }
    }
}
