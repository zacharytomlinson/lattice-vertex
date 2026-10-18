//! Release identity for the maintained empty-appliance OTA fixtures only.
//! The public key is supplied at build time; no private key or default trust key
//! is stored in the repository or ordinary kernel configuration.
pub fn policy(platform: u16) -> lattice_vertex::kernel::update::package::Policy {
    lattice_vertex::kernel::update::package::Policy {
        public_key: *include_bytes!(concat!(env!("OUT_DIR"), "/update-public-key.bin")),
        platform,
        layout: 2,
        appliance: [1; 16],
        surface: lattice_vertex::kernel::update::surface_identity(&lattice_vertex::surface::EMPTY),
        release: if cfg!(feature = "ota-image-b") { 2 } else { 1 },
    }
}
