//! Release identity for the maintained OTA fixtures and their actual surface.
//! The public key is supplied at build time; no private key or default trust key
//! is stored in the repository or ordinary kernel configuration.
pub fn policy(platform: u16, surface: [u8; 32]) -> lattice_vertex::kernel::update::package::Policy {
    lattice_vertex::kernel::update::package::Policy {
        public_key: *include_bytes!(concat!(env!("OUT_DIR"), "/update-public-key.bin")),
        platform,
        layout: 3,
        appliance: [1; 16],
        surface,
        release: u64::from_le_bytes(*include_bytes!(concat!(
            env!("OUT_DIR"),
            "/update-release.bin"
        ))),
    }
}
