#[allow(dead_code)]
#[path = "src/layout.rs"]
mod layout;

fn main() {
    println!("cargo:rerun-if-changed=src/layout.rs");
    if std::env::var_os("CARGO_FEATURE_LOADER").is_none() {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let l = layout::XIAO;
    std::fs::write(out.join("memory.x"), format!(
        "MEMORY {{ FLASH : ORIGIN = {:#x}, LENGTH = {:#x}\nRAM : ORIGIN = {:#x}, LENGTH = 256K }}\n_stack_start = ORIGIN(RAM) + LENGTH(RAM);\n",
        l.loader_start, l.state_start - l.loader_start, layout::RAM_START,
    )).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg-bin=lattice-nrf52840-loader=-Tlink.x");
}
