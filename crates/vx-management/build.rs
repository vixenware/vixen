use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vixen-management-Info.plist");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let info_plist = manifest_dir.join("vixen-management-Info.plist");
    println!(
        "cargo:rustc-link-arg-bin=vixen-management=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        info_plist.display()
    );
}
