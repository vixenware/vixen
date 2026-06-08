//! Explicit VixenFS uninstall cleanup.
//!
//! This is not part of the install or repair path. The app queues it only after the
//! user chooses the uninstall action; the already-approved root service performs the
//! privileged cleanup and removes itself last.

use std::fs;
use std::path::Path;
use std::process::Command;

use eyre::{Result, WrapErr};

use crate::{mount, queue};

const SUPPORT_ROOT: &str = "/Library/Application Support/Vixen";
const LEGACY_LAUNCH_DAEMON: &str = "/Library/LaunchDaemons/eu.bearcove.vixen.management.plist";
const LEGACY_LAUNCH_DAEMON_TARGET: &str = "system/eu.bearcove.vixen.management";
const INSTALLED_APP_PATH: &str = "/Applications/Vixen.app";

pub fn uninstall_vixenfs() -> Result<()> {
    tracing::info!("VixenFS uninstall cleanup started");
    let legacy_launch_daemon_present = Path::new(LEGACY_LAUNCH_DAEMON).exists();
    mount::unmount().wrap_err("unmounting VixenFS before uninstall cleanup")?;
    if let Ok(image) = mount::image_path() {
        remove_file_if_present(&image.to_string_lossy())?;
    }
    remove_file_if_present(LEGACY_LAUNCH_DAEMON)?;
    remove_dir_if_present(INSTALLED_APP_PATH)?;
    remove_dir_if_present(queue::SUPPORT_DIR)?;
    remove_empty_dir_if_present(SUPPORT_ROOT)?;
    bootout_legacy_launch_daemon(legacy_launch_daemon_present);
    tracing::info!("VixenFS uninstall cleanup finished");
    Ok(())
}

fn bootout_legacy_launch_daemon(was_installed: bool) {
    if !was_installed {
        tracing::info!(
            path = LEGACY_LAUNCH_DAEMON,
            "legacy Vixen LaunchDaemon absent"
        );
        return;
    }
    let output = Command::new("/bin/launchctl")
        .args(["bootout", LEGACY_LAUNCH_DAEMON_TARGET])
        .output();
    match output {
        Ok(output) => {
            tracing::info!(
                status = ?output.status.code(),
                stdout = %String::from_utf8_lossy(&output.stdout).trim(),
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "legacy Vixen LaunchDaemon bootout finished"
            );
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to boot out legacy Vixen LaunchDaemon");
        }
    }
}

fn remove_file_if_present(path: &str) -> Result<()> {
    let path = Path::new(path);
    match fs::remove_file(path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "removed Vixen uninstall file");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "Vixen uninstall file already absent");
            Ok(())
        }
        Err(err) => Err(err).wrap_err_with(|| format!("removing {}", path.display())),
    }
}

fn remove_dir_if_present(path: &str) -> Result<()> {
    let path = Path::new(path);
    match fs::remove_dir_all(path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "removed Vixen uninstall directory");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "Vixen uninstall directory already absent");
            Ok(())
        }
        Err(err) => Err(err).wrap_err_with(|| format!("removing {}", path.display())),
    }
}

fn remove_empty_dir_if_present(path: &str) -> Result<()> {
    let path = Path::new(path);
    match fs::remove_dir(path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "removed empty Vixen support root");
            Ok(())
        }
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound
                || err.kind() == std::io::ErrorKind::DirectoryNotEmpty =>
        {
            tracing::info!(path = %path.display(), "Vixen support root left in place");
            Ok(())
        }
        Err(err) => Err(err).wrap_err_with(|| format!("removing empty {}", path.display())),
    }
}
