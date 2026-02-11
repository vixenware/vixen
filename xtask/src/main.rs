use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("install") => install(),
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!("Available commands: install");
            std::process::exit(1);
        }
        None => {
            eprintln!("Usage: cargo xtask <command>");
            eprintln!("Available commands: install");
            std::process::exit(1);
        }
    }
}

fn install() {
    let binaries = vec!["vx", "vx-store", "vx-runner", "vx-vfsd", "vxd"];

    // Find workspace root
    let workspace_root = find_workspace_root().expect("Could not find workspace root");

    println!("Building vixen binaries...");
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&workspace_root)
        .status()
        .expect("Failed to run cargo build");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let release_dir = workspace_root.join("target/release");

    // Determine installation directory
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("Neither HOME nor USERPROFILE is set");
    let install_dir = PathBuf::from(home).join(".cargo").join("bin");

    for binary_name in binaries {
        let src = if cfg!(windows) {
            release_dir.join(format!("{binary_name}.exe"))
        } else {
            release_dir.join(binary_name)
        };
        let dst = if cfg!(windows) {
            install_dir.join(format!("{binary_name}.exe"))
        } else {
            install_dir.join(binary_name)
        };

        if !src.exists() {
            eprintln!(
                "Warning: {} not found in target/release, skipping",
                binary_name
            );
            continue;
        }

        fs::copy(&src, &dst).unwrap_or_else(|_| panic!("Failed to copy {}", binary_name));
        println!("Copied {} to {}", binary_name, dst.display());

        // On macOS, codesign the installed binary to avoid AMFI issues
        // (signing must happen AFTER copy, not before)
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

        // Verify the installed binary works (only for binaries that support --version
        // without needing environment variables like VX_HOME)
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

/// Find the workspace root by walking up from the current directory
/// looking for a Cargo.toml with `[workspace]`.
fn find_workspace_root() -> Option<PathBuf> {
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
