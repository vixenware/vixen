use std::fmt::Write as _;

use camino::Utf8PathBuf;
use moire::process::Command;

use crate::Result;

const VIXEN_EXTENSION_IDENTIFIER: &str = "eu.bearcove.vixenfs";
const INSTALLED_APPEX_PATH: &str = "/Applications/Vixen.app/Contents/Extensions/VixenFS.appex";

pub async fn cleanup_stuck_mount(_mount_path: &Utf8PathBuf) -> Result<()> {
    // Noop on macOS — vx_fskit::setup() handles all pre-mount cleanup.
    Ok(())
}

pub async fn collect_vfs_diagnostics(mount_path: &Utf8PathBuf) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "=== macOS VFS diagnostics ===");
    let _ = writeln!(out, "mount path: {mount_path}");
    let _ = writeln!(out, "expected installed appex: {INSTALLED_APPEX_PATH}");

    append_command_output(
        &mut out,
        "pluginkit registration",
        "/usr/bin/pluginkit",
        &["-mAvvv", "-i", VIXEN_EXTENSION_IDENTIFIER],
    )
    .await;

    let registration = capture_stdout(
        "/usr/bin/pluginkit",
        &["-mAvvv", "-i", VIXEN_EXTENSION_IDENTIFIER],
    )
    .await;
    append_registration_summary(&mut out, registration.as_deref());

    append_command_output(
        &mut out,
        "fskit module list",
        "/usr/bin/pluginkit",
        &["-m", "-p", "com.apple.fskit.fsmodule"],
    )
    .await;

    append_command_output(
        &mut out,
        "running VixenFS processes",
        "/usr/bin/pgrep",
        &["-ax", "VixenFS"],
    )
    .await;

    append_command_output(
        &mut out,
        "recent extension logs",
        "/usr/bin/log",
        &[
            "show",
            "--last",
            "20s",
            "--style",
            "compact",
            "--predicate",
            "process == \"mount\" OR process == \"fskitd\" OR process == \"fskit_agent\" OR process == \"extensionkitservice\" OR eventMessage CONTAINS \"eu.bearcove.vixenfs\"",
        ],
    )
    .await;

    out
}

/// Whether we should create the mount directory.
pub fn should_create_mount_dir(_mount_path: &Utf8PathBuf) -> bool {
    // vx-vfs creates the macOS mountpoint with a single final mkdir to avoid
    // recursively probing an existing FSKit mount.
    false
}

async fn append_command_output(out: &mut String, title: &str, program: &str, args: &[&str]) {
    let _ = writeln!(out);
    let _ = writeln!(out, "=== {title} ===");
    let _ = writeln!(out, "$ {} {}", program, args.join(" "));

    match Command::new(program).args(args).output().await {
        Ok(output) => {
            let _ = writeln!(out, "exit: {}", output.status);

            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.trim().is_empty() {
                let _ = writeln!(out, "(stdout empty)");
            } else {
                let _ = writeln!(out, "{stdout}");
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                let _ = writeln!(out, "--- stderr ---");
                let _ = writeln!(out, "{stderr}");
            }
        }
        Err(error) => {
            let _ = writeln!(out, "failed to run: {error}");
        }
    }
}

async fn capture_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().await.ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn append_registration_summary(out: &mut String, registration: Option<&str>) {
    let _ = writeln!(out);
    let _ = writeln!(out, "=== registration summary ===");

    let Some(registration) = registration else {
        let _ = writeln!(out, "could not read pluginkit registration output");
        return;
    };

    let appex_paths = registration
        .lines()
        .filter_map(extract_appex_path)
        .collect::<Vec<_>>();

    if appex_paths.is_empty() {
        let _ = writeln!(out, "no appex paths found in registration output");
        return;
    }

    for path in &appex_paths {
        let _ = writeln!(out, "registered appex: {path}");
    }

    if appex_paths.iter().any(|path| path == INSTALLED_APPEX_PATH) {
        let _ = writeln!(out, "installed app registration is present");
    } else {
        let _ = writeln!(out, "installed app registration is missing");
    }

    let derived_data_paths = appex_paths
        .iter()
        .filter(|path| path.contains("/DerivedData/"))
        .collect::<Vec<_>>();
    if !derived_data_paths.is_empty() {
        let _ = writeln!(out, "warning: DerivedData registration is present");
        for path in derived_data_paths {
            let _ = writeln!(out, "  derived data appex: {path}");
        }
    }
}

fn extract_appex_path(line: &str) -> Option<String> {
    let start = line.find('/')?;
    let path = &line[start..];
    let end = path.find(".appex")?;
    Some(path[..end + ".appex".len()].to_owned())
}

#[cfg(test)]
mod tests {
    use super::{INSTALLED_APPEX_PATH, extract_appex_path};

    #[test]
    fn extract_appex_path_finds_installed_path() {
        let line = format!(
            "    path = {} uuid = 01234567-89AB-CDEF-0123-456789ABCDEF",
            INSTALLED_APPEX_PATH
        );
        assert_eq!(
            extract_appex_path(&line).as_deref(),
            Some(INSTALLED_APPEX_PATH)
        );
    }

    #[test]
    fn extract_appex_path_finds_derived_data_path() {
        let line = "foo /Users/amos/Library/Developer/Xcode/DerivedData/Vixen-abc/Build/Products/Debug/Vixen.app/Contents/Extensions/VixenFS.appex bar";
        assert_eq!(
            extract_appex_path(line).as_deref(),
            Some(
                "/Users/amos/Library/Developer/Xcode/DerivedData/Vixen-abc/Build/Products/Debug/Vixen.app/Contents/Extensions/VixenFS.appex"
            )
        );
    }
}
