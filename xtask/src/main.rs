use std::fs;
use std::path::PathBuf;
use std::process::Command;

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
        Some("codegen") => codegen(),
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!(
                "Available commands: install, vixenfs-codegen, bump-roam, perfetto-proto, smoke, integration, registry-vx"
            );
            std::process::exit(1);
        }
        None => {
            eprintln!("Usage: cargo xtask <command>");
            eprintln!("Available commands: install");
            std::process::exit(1);
        }
    }
}

/// Regenerate the Swift vox bindings for the macOS app's VixenFS backend.
///
/// The FSKit extension serves the volume by calling the `Vfs` service on whoever connects
/// to its socket; the app implements that service in Swift (`VfsHandler`). This emits the
/// `VfsDispatcher`/`VfsHandler`/types from the `Vfs` service descriptor via `vox-codegen`
/// (pinned to the same vox 0.8.2 the extension speaks). Mirrors bee's `bee-services-codegen`.
fn codegen() {
    let workspace_root = find_workspace_root().expect("Could not find workspace root");
    let out_dir = workspace_root.join("apps/mac/xcode/Vixen/Generated");
    fs::create_dir_all(&out_dir).expect("creating Generated dir");

    let code = vox_codegen::targets::swift::generate_service(vx_services::vfs_service_descriptor());
    let out_path = out_dir.join("GeneratedVfsService.swift");
    fs::write(&out_path, code).expect("writing generated Swift");
    println!("Wrote {}", out_path.display());
}

fn install(mode: InstallMode) {
    let binaries = vec!["vx", "vixen"];

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

        if binary_name == "vixen" {
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
