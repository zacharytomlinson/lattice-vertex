//! A small native-only lamp. Mathematical models are optional, not generated.
//! Run: cargo run --no-default-features --features port-test,use-mutexes --example surface
use lattice_vertex::{
    kernel::{Appliance, State, StateChangeError},
    surface::{StateHandle, Surface, TransformerHandle},
    sync::TaskContext,
};

struct Lamp {
    power: State<bool>,
    brightness: State<u8>,
}
#[derive(Debug)]
enum Error {
    Brightness,
    State,
}
impl From<StateChangeError> for Error {
    fn from(_: StateChangeError) -> Self {
        Self::State
    }
}
impl Lamp {
    fn set_brightness(&self, brightness: u8) -> Result<(), Error> {
        if brightness > 200 {
            return Err(Error::Brightness);
        }
        // Real firmware applies its calibrated electrical output here.
        self.brightness.set(brightness)?;
        self.power.set(brightness != 0)?;
        Ok(())
    }
}

// Declare each field once. Initialization performs the kernel wiring.
static POWER: StateHandle<Lamp> = StateHandle::new("power", |lamp| &lamp.power);
static BRIGHTNESS: StateHandle<Lamp> = StateHandle::new("level", |lamp| &lamp.brightness);
static SET_BRIGHTNESS: TransformerHandle<Lamp, Error> =
    TransformerHandle::from_fn("set-level", Lamp::set_brightness);
static SURFACE: Surface<Lamp, Error> = Surface::new(&[&POWER, &BRIGHTNESS], &[&SET_BRIGHTNESS]);
static LAMP: Appliance<Lamp, Error> = Appliance::new(&SURFACE);

fn main() {
    // The selected CPU port establishes this context in a real board entry.
    let context = unsafe { TaskContext::assume() };
    LAMP.initialize(
        &context,
        Lamp {
            power: State::new(false),
            brightness: State::new(0),
        },
    );
    LAMP.call(|lamp| lamp.set_brightness(128)).unwrap();
    println!(
        "Power: {:?}; brightness: {:?}; IR: {:?}",
        LAMP.device().power.get(),
        LAMP.device().brightness.get(),
        SURFACE.ir_status()
    );
    // Production now enters kernel::start (or its storage/update variant).
    // This host example does not run RTOS tasks or claim radio delivery.
}
