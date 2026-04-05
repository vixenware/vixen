use std::fs;
use std::path::PathBuf;
use std::process::Command;

use eyre::{Context, ContextCompat, Result};

enum InstallMode {
    Release,
    Dev,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("install") => {
            let mode = match args.get(1).map(|s| s.as_str()) {
                Some("--dev") => InstallMode::Dev,
                _ => InstallMode::Release,
            };
            install(mode)
        }
        Some("perfetto-proto") => {
            if let Err(error) = perfetto_proto() {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!(
                "Available commands: install, vixenfs-codegen, bump-roam, perfetto-proto, smoke, integration, registry-vx"
            );
            std::process::exit(1);
        }
        None => {
            eprintln!("Usage: cargo xtask <command>");
            eprintln!(
                "Available commands: install, vixenfs-codegen, bump-roam, perfetto-proto, smoke, integration, registry-vx"
            );
            std::process::exit(1);
        }
    }
}

fn install(mode: InstallMode) {
    let binaries = vec!["vx"];

    // Find workspace root
    let workspace_root = find_workspace_root().expect("Could not find workspace root");

    println!("Building vixen binaries...");
    let mut build_args = vec!["build"];
    if matches!(mode, InstallMode::Release) {
        build_args.push("--release");
    }
    let status = Command::new("cargo")
        .args(&build_args)
        .current_dir(&workspace_root)
        .status()
        .expect("Failed to run cargo build");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let target_dir = match mode {
        InstallMode::Release => workspace_root.join("target/release"),
        InstallMode::Dev => workspace_root.join("target/debug"),
    };

    // Determine installation directory
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("Neither HOME nor USERPROFILE is set");
    let install_dir = PathBuf::from(home).join(".cargo").join("bin");

    for binary_name in binaries {
        let src = if cfg!(windows) {
            target_dir.join(format!("{binary_name}.exe"))
        } else {
            target_dir.join(binary_name)
        };
        let dst = if cfg!(windows) {
            install_dir.join(format!("{binary_name}.exe"))
        } else {
            install_dir.join(binary_name)
        };

        if !src.exists() {
            eprintln!(
                "Warning: {} not found in {}, skipping",
                binary_name,
                target_dir.display()
            );
            continue;
        }

        fs::copy(&src, &dst).unwrap_or_else(|_| panic!("Failed to copy {}", binary_name));
        println!("Copied {} to {}", binary_name, dst.display());

        #[cfg(target_os = "macos")]
        {
            let dst_str = dst.to_str().expect("non-UTF-8 path");
            let status = Command::new("codesign")
                .args(["--sign", "-", "--force", dst_str])
                .status()
                .expect("Failed to run codesign");

            if !status.success() {
                eprintln!(
                    "Warning: codesign failed for {}, continuing anyway",
                    binary_name
                );
            }
        }

        if binary_name == "vx" {
            let output = Command::new(&dst)
                .arg("--version")
                .output()
                .unwrap_or_else(|_| panic!("Failed to run {} --version", binary_name));

            if !output.status.success() {
                eprintln!("Error: {} --version failed", binary_name);
                eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                std::process::exit(1);
            }

            let version = String::from_utf8_lossy(&output.stdout);
            println!("Installed: {}", version.trim());
        } else {
            println!("Installed {}", binary_name);
        }
    }
}

fn perfetto_proto() -> Result<()> {
    let workspace_root = find_workspace_root().context("Could not find workspace root")?;
    let proto_root = workspace_root.join("third_party/perfetto-proto");
    let proto_file = proto_root.join("perfetto_trace.proto");
    let output_file = workspace_root.join("crates/perfetto-proto/lib.rs");
    let output_dir = tempfile::tempdir().context("failed to create temp output dir")?;
    let protoc = protoc_bin_vendored::protoc_bin_path().context("failed to resolve protoc")?;

    // This short-lived xtask process sets PROTOC before invoking prost-build and
    // does not concurrently mutate environment variables elsewhere.
    unsafe {
        std::env::set_var("PROTOC", &protoc);
    }

    prost_build::Config::new()
        .bytes(["."])
        .out_dir(output_dir.path())
        .compile_protos(&[proto_file.as_path()], &[proto_root.as_path()])
        .context("failed to compile Perfetto proto schema")?;

    let generated = output_dir.path().join("perfetto.protos.rs");
    let generated_source =
        fs::read_to_string(&generated).context("failed to read generated Perfetto schema")?;
    let mut output = String::from(
        "// @generated by `cargo xtask perfetto-proto`\n\
         // Source: third_party/perfetto-proto/perfetto_trace.proto\n\
         // Do not edit by hand.\n\n",
    );
    output.push_str(&generated_source);
    fs::write(&output_file, output).context("failed to write generated Perfetto schema")?;

    let status = Command::new("rustfmt")
        .arg(&output_file)
        .current_dir(&workspace_root)
        .status()
        .context("failed to run rustfmt on generated Perfetto schema")?;
    if !status.success() {
        eyre::bail!("rustfmt failed for {}", output_file.display());
    }

    println!("Wrote {}", output_file.display());
    Ok(())
}

/// Find the workspace root by walking up from the current directory
/// looking for a Cargo.toml with `[workspace]`.
pub(crate) fn find_workspace_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;

    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists()
            && let Ok(contents) = std::fs::read_to_string(&cargo_toml)
            && contents.contains("[workspace]")
        {
            return Some(dir);
        }

        if !dir.pop() {
            return None;
        }
    }
}
