//! Kernel-owned GPIO edge handlers. No software input mirror or poll/compare loop.
use crate::kernel::{state, Edge, Interrupt, Pull};
use esp_hal::{
    gpio::{Event, Input, InputConfig, InputPin},
    interrupt::{self, Priority},
};
struct Handler {
    input: Input<'static>,
    action: fn(),
    edge: Event,
    enabled: bool,
}
// ESP32-C6 has GPIO0..GPIO30. A pin token can enter this owner only once.
static mut HANDLERS: [Option<Handler>; 31] = [const { None }; 31];
static mut STARTED: bool = false;
/// Consume an input before startup. The bounded, nonblocking action runs in the
/// GPIO IRQ; use State for mutations and kernel work/timers for later work.
/// The kernel clears interrupt status. It owns the GPIO interrupt dispatcher;
/// do not install another dispatcher through the HAL.
pub fn on_edge(pin: impl InputPin + 'static, pull: Pull, edge: Edge, action: fn()) -> Interrupt {
    state::atomic(|| unsafe {
        assert!(!STARTED, "declare inputs before kernel startup");
        let i = pin.number() as usize;
        let pull = match pull {
            Pull::None => esp_hal::gpio::Pull::None,
            Pull::Up => esp_hal::gpio::Pull::Up,
            Pull::Down => esp_hal::gpio::Pull::Down,
        };
        let mut input = Input::new(pin, InputConfig::default().with_pull(pull));
        input.unlisten();
        input.clear_interrupt();
        let edge = match edge {
            Edge::Rising => Event::RisingEdge,
            Edge::Falling => Event::FallingEdge,
            Edge::Either => Event::AnyEdge,
        };
        HANDLERS[i] = Some(Handler {
            input,
            action,
            edge,
            enabled: true,
        });
        Interrupt::new(i as u8, set, read)
    })
}
fn read(index: u8) -> bool {
    state::atomic(|| unsafe {
        (&*core::ptr::addr_of!(HANDLERS))[index as usize]
            .as_ref()
            .unwrap()
            .input
            .is_high()
    })
}
fn set(index: u8, enabled: bool) {
    state::atomic(|| unsafe {
        let h = (&mut *core::ptr::addr_of_mut!(HANDLERS))[index as usize]
            .as_mut()
            .unwrap();
        h.enabled = enabled;
        h.input.unlisten();
        h.input.clear_interrupt();
        if enabled && STARTED {
            h.input.listen(h.edge);
        }
    });
}
pub(crate) fn start() {
    state::atomic(|| unsafe {
        STARTED = true;
        interrupt::bind_handler(esp_hal::peripherals::Interrupt::GPIO, GPIO);
        for i in 0..31 {
            if let Some(h) = (&*core::ptr::addr_of!(HANDLERS))[i].as_ref() {
                set(i as u8, h.enabled);
            }
        }
    });
}
#[esp_hal::handler(priority = Priority::Priority2)]
fn GPIO() {
    for i in 0..31 {
        let action = state::atomic(|| unsafe {
            let h = (&mut *core::ptr::addr_of_mut!(HANDLERS))[i].as_mut()?;
            if !h.enabled || !h.input.is_interrupt_set() {
                return None;
            }
            h.input.clear_interrupt();
            Some(h.action)
        });
        if let Some(action) = action {
            state::atomic(action);
        }
    }
}
