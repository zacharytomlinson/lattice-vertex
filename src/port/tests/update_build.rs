//! Build-only helper for OTA fixtures; accepts only a supplied public key.
pub fn policy(out: &std::path::Path) {
    println!("cargo:rerun-if-env-changed=LATTICE_UPDATE_PUBLIC_KEY");
    println!("cargo:rerun-if-env-changed=LATTICE_UPDATE_RELEASE");
    if std::env::var_os("CARGO_FEATURE_OTA_TEST").is_none() {
        return;
    }
    let key = std::env::var("LATTICE_UPDATE_PUBLIC_KEY")
        .expect("supply LATTICE_UPDATE_PUBLIC_KEY (64 hex digits) for the OTA fixture");
    assert_eq!(key.len(), 64, "Ed25519 public key must contain 32 bytes");
    let bytes: Vec<_> = key
        .as_bytes()
        .chunks_exact(2)
        .map(|b| {
            u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).expect("invalid public key hex")
        })
        .collect();
    std::fs::write(out.join("update-public-key.bin"), bytes).unwrap();
    // Repeated OTA checks can advance a real running release without reprovisioning
    // its bootloader or lying about the candidate's compiled release identity.
    let default = if std::env::var_os("CARGO_FEATURE_OTA_IMAGE_B").is_some() {
        2
    } else {
        1
    };
    let release = std::env::var("LATTICE_UPDATE_RELEASE")
        .map(|s| s.parse::<u64>().expect("invalid LATTICE_UPDATE_RELEASE"))
        .unwrap_or(default);
    assert!(release > 0, "release must be positive");
    std::fs::write(out.join("update-release.bin"), release.to_le_bytes()).unwrap();
}
