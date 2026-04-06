#[cfg(target_os = "macos")]
fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let output_file = std::path::PathBuf::from(&out_dir).join("RustFSKitBridge.h");

    let config =
        cbindgen::Config::from_file(std::path::Path::new(&crate_dir).join("cbindgen.toml"))
            .expect("failed to read cbindgen.toml");

    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
        .expect("cbindgen failed to generate bindings")
        .write_to_file(&output_file);

    println!("cargo::rerun-if-changed=src/");
    println!("cargo::rerun-if-changed=cbindgen.toml");
    // Tell downstream (Xcode build script) where to find the header
    println!("cargo::metadata=include={out_dir}");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
