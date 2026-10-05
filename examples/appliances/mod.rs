//! Appliance-owned reference surfaces. No device policy is added to the kernel.

use lattice_vertex::ir::{Integer, Value};

// Only repetitive trait glue; the state remains in the concrete appliance.
macro_rules! state {
    ($name:ident, $handle:ident, $appliance:ty, $value:ty, $read:expr) => {
        pub struct $name;
        impl lattice_vertex::surface::State for $name {
            type Appliance = $appliance;
            type Value = $value;
            fn read(appliance: &$appliance) -> Option<$value> {
                ($read)(appliance)
            }
        }
        pub static $handle: lattice_vertex::surface::StateHandle<$appliance> =
            lattice_vertex::surface::StateHandle::new::<$name>();
    };
}

const fn integer(ty: Integer, value: i128) -> Value {
    match Value::integer(ty, value) {
        Ok(value) => value,
        Err(_) => panic!("invalid appliance-model literal"),
    }
}

pub mod diffuser;
pub mod radio_fan;
pub mod shelly_dimmer;
