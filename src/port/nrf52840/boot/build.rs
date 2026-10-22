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
        "MEMORY {{ FLASH : ORIGIN = {:#x}, LENGTH = {:#x}\nLAYOUT : ORIGIN = {:#x}, LENGTH = 64\nRAM : ORIGIN = {:#x}, LENGTH = 256K }}\n_stack_start = ORIGIN(RAM) + LENGTH(RAM);\n",
        l.loader_start, layout::DESCRIPTOR_ADDRESS - l.loader_start, layout::DESCRIPTOR_ADDRESS, layout::RAM_START,
    )).unwrap();
    std::fs::write(out.join("layout.x"), "SECTIONS { .lattice_layout : { KEEP(*(.lattice_layout)); } > LAYOUT } INSERT AFTER .rodata;\n").unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg-bin=lattice-nrf52840-loader=-Tlink.x");
    println!("cargo:rustc-link-arg-bin=lattice-nrf52840-loader=-Tlayout.x");
}
