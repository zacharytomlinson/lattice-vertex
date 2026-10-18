#[path = "../../tests/update_build.rs"]
mod update_build;
fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    update_build::policy(&out);
}
