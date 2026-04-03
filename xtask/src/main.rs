use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;

use eyre::{Context, ContextCompat, Result};

mod integration;
mod registry_vx;

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
        Some("vixenfs-codegen") => {
            if let Err(error) = vixenfs_codegen() {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Some("bump-roam") => bump_roam(args.get(1).map(|s| s.as_str())),
        Some("perfetto-proto") => {
            if let Err(error) = perfetto_proto() {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Some("smoke") => smoke(&args[1..]),
        Some("integration") => {
            if let Err(error) = integration::run(&args[1..]) {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Some("registry-vx") => {
            if let Err(error) = registry_vx::run(&args[1..]) {
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
    let binaries = vec!["vx", "vx-runner", "vx-vfsd"];

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

fn vixenfs_codegen() -> Result<(), eyre::Error> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| eyre::eyre!("failed to locate workspace root"))?
        .to_path_buf();

    let service = vx_services::vfs_service_descriptor();
    let swift = vox_codegen::targets::swift::generate_service_with_bindings(
        service,
        vox_codegen::targets::swift::SwiftBindings::Client,
    );

    let output = workspace_root.join("apps/mac/xcode/VixenFS/VfsClient.swift");

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, &swift)?;
    println!("Wrote {}", output.display());

    Ok(())
}

fn bump_roam(rev: Option<&str>) {
    let workspace_root = find_workspace_root().expect("Could not find workspace root");
    let mac_dir = workspace_root.join("apps/mac");

    let new_rev = match rev {
        Some(r) => r.to_string(),
        None => {
            let output = Command::new("git")
                .args(["ls-remote", "https://github.com/bearcove/roam.git", "HEAD"])
                .output()
                .expect("Failed to run git ls-remote");
            if !output.status.success() {
                eprintln!("Failed to fetch latest roam revision");
                std::process::exit(1);
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout
                .split_whitespace()
                .next()
                .expect("Empty output from git ls-remote")
                .to_string()
        }
    };

    println!("Bumping roam to revision: {}", new_rev);

    let project_yml_path = mac_dir.join("xcode/project.yml");
    let package_swift_path = mac_dir.join("Package.swift");

    for path in [&project_yml_path, &package_swift_path] {
        if !path.exists() {
            eprintln!("Warning: {} does not exist, skipping", path.display());
            continue;
        }

        let content = fs::read_to_string(path).expect("Failed to read file");
        let mut new_content = String::new();
        for line in content.lines() {
            if line.contains("revision:") {
                let prefix = line.split("revision:").next().unwrap();
                if path.ends_with("project.yml") {
                    new_content.push_str(prefix);
                    new_content.push_str(&format!("revision: {}", new_rev));
                } else {
                    new_content.push_str(prefix);
                    let suffix = if line.trim_end().ends_with(',') {
                        "\","
                    } else {
                        "\""
                    };
                    new_content.push_str(&format!("revision: \"{}{}", new_rev, suffix));
                }
                new_content.push('\n');
            } else {
                new_content.push_str(line);
                new_content.push('\n');
            }
        }

        fs::write(path, new_content).expect("Failed to write file");
        println!("Updated {}", path.display());
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

fn smoke(raw_args: &[String]) {
    let (build_file, extra_args) = match raw_args.split_first() {
        Some((build_file, extra_args)) => (build_file, extra_args),
        None => {
            eprintln!("Usage: cargo xtask smoke <build.vx> [-- <extra vx build args>]");
            std::process::exit(1);
        }
    };
    let extra_args = match extra_args.split_first() {
        Some((dash_dash, rest)) if dash_dash == "--" => rest,
        _ => extra_args,
    };

    let workspace_root = find_workspace_root().expect("Could not find workspace root");
    let build_status = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .current_dir(&workspace_root)
        .status()
        .expect("Failed to run cargo build for smoke test");
    if !build_status.success() {
        std::process::exit(build_status.code().unwrap_or(1));
    }

    let vx_home = std::env::var("VX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("vx-smoke-home"));

    prepare_smoke_home(&vx_home).expect("Failed to prepare smoke VX_HOME");

    let mut cmd = Command::new(workspace_root.join("target/release/vx"));
    cmd.arg("build")
        .arg("--release")
        .arg("--no-tui")
        .arg("-f")
        .arg(build_file);
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.current_dir(&workspace_root)
        .env("VX_HOME", &vx_home)
        .env("VX_TEST", "1");
    eprintln!("Launching: {cmd:?}");

    let status = cmd.status().expect("Failed to run vx smoke build");
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn prepare_smoke_home(vx_home: &PathBuf) -> Result<()> {
    kill_smoke_vxd(vx_home).context("failed to stop previous smoke vxd")?;

    if vx_home.exists() {
        fs::remove_dir_all(vx_home).with_context(|| {
            format!("failed to remove previous smoke home {}", vx_home.display())
        })?;
    }

    Ok(())
}

#[cfg(unix)]
fn kill_smoke_vxd(vx_home: &PathBuf) -> Result<()> {
    let needle = vx_home.display().to_string();
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,args="])
        .output()
        .context("failed to enumerate processes with ps")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut vxd_pids = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(pid) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(_ppid) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let args = parts.collect::<Vec<_>>().join(" ");
        if args.contains(&needle) && args.contains("vxd") {
            vxd_pids.push(pid);
        }
    }

    if vxd_pids.is_empty() {
        return Ok(());
    }

    eprintln!(
        "Stopping previous smoke vxd for {}: {:?}",
        vx_home.display(),
        vxd_pids
    );

    for pid in &vxd_pids {
        let status = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .with_context(|| format!("failed to signal pid {pid}"))?;
        if !status.success() {
            eyre::bail!("kill -TERM {pid} failed with status {status}");
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let mut alive = Vec::new();
        for pid in &vxd_pids {
            let status = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .with_context(|| format!("failed to probe pid {pid}"))?;
            if status.success() {
                alive.push(*pid);
            }
        }
        if alive.is_empty() {
            thread::sleep(std::time::Duration::from_millis(100));
            return Ok(());
        }
        thread::sleep(std::time::Duration::from_millis(100));
    }

    for pid in &vxd_pids {
        let status = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .with_context(|| format!("failed to probe pid {pid}"))?;
        if !status.success() {
            continue;
        }
        let status = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .with_context(|| format!("failed to force-kill pid {pid}"))?;
        if !status.success() {
            eyre::bail!("kill -KILL {pid} failed with status {status}");
        }
    }

    thread::sleep(std::time::Duration::from_millis(200));
    Ok(())
}

#[cfg(not(unix))]
fn kill_smoke_vxd(_vx_home: &PathBuf) -> Result<()> {
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
