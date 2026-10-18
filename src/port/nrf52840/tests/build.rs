#[path = "../../tests/update_build.rs"]
mod update_build;
#[allow(dead_code)]
#[path = "../boot/src/layout.rs"]
mod update_layout;

fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    update_build::policy(&out);
    if std::env::var_os("CARGO_FEATURE_UPDATE_TEST").is_some() {
        let l = update_layout::XIAO;
        std::fs::write(out.join("memory.x"), format!(
            "MEMORY {{ FLASH : ORIGIN = {:#x}, LENGTH = {:#x}\nRAM : ORIGIN = {:#x}, LENGTH = 256K }}\n_stack_start = ORIGIN(RAM) + LENGTH(RAM);\n_stack_end = _stack_start - 4K;\n",
            l.active_start, l.image_capacity(), update_layout::RAM_START,
        )).unwrap();
        println!("cargo:rustc-link-search={}", out.display());
        println!("cargo:rerun-if-changed=../boot/src/layout.rs");
        return;
    }
    let layout = if std::env::var_os("CARGO_FEATURE_XIAO_BOOTLOADER").is_some() {
        "memory-xiao.x"
    } else {
        "memory-standalone.x"
    };
    std::fs::copy(layout, out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed={layout}");
}
