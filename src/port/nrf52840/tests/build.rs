fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let layout = if std::env::var_os("CARGO_FEATURE_XIAO_BOOTLOADER").is_some() {
        "memory-xiao.x"
    } else {
        "memory-standalone.x"
    };
    std::fs::copy(layout, out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed={layout}");
}
