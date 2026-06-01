//! Establishing the block-backed `vixenfs` volume.
//!
//! macOS 15+ supports only block-device FSKit resources, so the CAS-VFS is mounted on
//! a **mock raw disk image**: a sparse file attached as a raw block device whose bytes
//! are never read. File data is still served by the extension from the CAS over Vox —
//! the device is only what FSKit binds the extension through. Being a block volume, it
//! mounts under `/Volumes` as a *local* volume, free of the generic-URL TCC gate that
//! forced Full Disk Access before.
//!
//! `mount(2)` needs root, so these run inside the privileged daemon. Provisioning the
//! image + `hdiutil attach` is unprivileged, but the daemon owns the whole sequence so
//! its trigger stays payload-less (no caller names the device or path).

use std::process::Command;

use eyre::{Result, WrapErr, bail};

/// Where the block volume mounts — a real `/Volumes` volume, not an arbitrary path.
pub const MOUNT_POINT: &str = "/Volumes/Vixen";
/// The FSKit filesystem type — matches the extension's `FSFileSystemType`.
pub const FS_TYPE: &str = "vixenfs";
/// The mock backing image: sparse, root-owned, and never holds file data.
pub const IMAGE_PATH: &str = "/Library/Application Support/Vixen/vixen-volume.img";
/// Nominal size of the mock image. `mkfile -n` keeps it sparse (~0 real bytes); the
/// volume's reported size comes from the extension's statfs, not from this.
pub const IMAGE_SIZE: &str = "100m";

/// Establish the mount if it isn't already up: provision the device, then mount it.
/// Idempotent — a healthy existing mount is a no-op. **Requires root** (the mount).
pub fn establish() -> Result<()> {
    if is_mounted()? {
        tracing::info!("{MOUNT_POINT} already mounted");
        return Ok(());
    }
    let device = provision_device()?;
    mount_volume(&device)?;
    tracing::info!("mounted {device} at {MOUNT_POINT}");
    Ok(())
}

/// Force-unmount the volume if it is up. Idempotent. **Requires root.**
pub fn unmount() -> Result<()> {
    if !is_mounted()? {
        return Ok(());
    }
    // `diskutil unmount force` is the clean path for a `/Volumes` volume; fall back to
    // `umount -f` if diskutil declines.
    let out = Command::new("/usr/sbin/diskutil")
        .args(["unmount", "force", MOUNT_POINT])
        .output()
        .wrap_err("running diskutil unmount")?;
    if out.status.success() {
        tracing::info!("unmounted {MOUNT_POINT}");
        return Ok(());
    }
    let fallback = Command::new("/sbin/umount")
        .args(["-f", MOUNT_POINT])
        .output()
        .wrap_err("running umount -f")?;
    if !fallback.status.success() {
        bail!(
            "unmount {MOUNT_POINT} failed: {}",
            String::from_utf8_lossy(&fallback.stderr).trim()
        );
    }
    Ok(())
}

/// Provision the mock raw block device: create the sparse image if absent, attach it
/// raw with no filesystem mounted, and return its whole-disk BSD name (e.g. `disk18`).
pub fn provision_device() -> Result<String> {
    ensure_image()?;
    let out = Command::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-imagekey",
            "diskimage-class=CRawDiskImage",
            "-nomount",
            IMAGE_PATH,
        ])
        .output()
        .wrap_err("running hdiutil attach")?;
    if !out.status.success() {
        bail!(
            "hdiutil attach failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_attached_device(&stdout)
        .ok_or_else(|| eyre::eyre!("no whole-disk /dev/disk node in hdiutil output: {stdout:?}"))
}

/// Create the sparse mock image if it doesn't exist yet.
fn ensure_image() -> Result<()> {
    let path = std::path::Path::new(IMAGE_PATH);
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap_err("creating image support dir")?;
    }
    let out = Command::new("/usr/bin/mkfile")
        .args(["-n", IMAGE_SIZE, IMAGE_PATH])
        .output()
        .wrap_err("running mkfile")?;
    if !out.status.success() {
        bail!(
            "mkfile {IMAGE_PATH} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Mount the block device as `vixenfs` at [`MOUNT_POINT`]. **Requires root.**
fn mount_volume(device: &str) -> Result<()> {
    // `/Volumes/Vixen` mountpoint (root-owned). Already-exists is fine.
    std::fs::create_dir_all(MOUNT_POINT).ok();
    let out = Command::new("/sbin/mount")
        .args(["-F", "-t", FS_TYPE, device, MOUNT_POINT])
        .output()
        .wrap_err("running mount")?;
    if !out.status.success() {
        bail!(
            "mount {device} -> {MOUNT_POINT} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Is the vixenfs volume currently mounted at [`MOUNT_POINT`]?
pub fn is_mounted() -> Result<bool> {
    let out = Command::new("/sbin/mount")
        .output()
        .wrap_err("running mount")?;
    Ok(mount_table_has_volume(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// Extract the whole-disk BSD name (`disk18`) from `hdiutil attach` output, skipping
/// partition slices (`disk18s1`).
fn parse_attached_device(hdiutil_stdout: &str) -> Option<String> {
    for line in hdiutil_stdout.lines() {
        let Some(tok) = line.split_whitespace().next() else {
            continue;
        };
        let Some(name) = tok.strip_prefix("/dev/") else {
            continue;
        };
        // Whole disk = `disk` followed by digits only (a slice is `diskNsM`).
        if let Some(num) = name.strip_prefix("disk")
            && !num.is_empty()
            && num.chars().all(|c| c.is_ascii_digit())
        {
            return Some(name.to_string());
        }
    }
    None
}

/// Does the `mount` table list our vixenfs volume at [`MOUNT_POINT`]?
fn mount_table_has_volume(mount_output: &str) -> bool {
    mount_output.lines().any(|line| {
        line.contains(&format!(" on {MOUNT_POINT} "))
            && (line.contains(FS_TYPE) || line.contains("fskit"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whole_disk_from_hdiutil() {
        // Raw image, no partition map: a single whole-disk line.
        assert_eq!(
            parse_attached_device("/dev/disk18          \t\n"),
            Some("disk18".to_string())
        );
        // If a partition scheme appears, take the whole disk, not the slice.
        assert_eq!(
            parse_attached_device(
                "/dev/disk7          \tGUID_partition_scheme         \t\n/dev/disk7s1        \t\n"
            ),
            Some("disk7".to_string())
        );
        assert_eq!(parse_attached_device(""), None);
        assert_eq!(parse_attached_device("garbage output\n"), None);
    }

    #[test]
    fn detects_volume_in_mount_table() {
        assert!(mount_table_has_volume(
            "disk18 on /Volumes/Vixen (vixenfs, nodev, nosuid, fskit, mounted by amos)"
        ));
        assert!(!mount_table_has_volume(
            "/dev/disk1s1 on / (apfs, local, journaled)"
        ));
        // Right fs, wrong mountpoint.
        assert!(!mount_table_has_volume(
            "disk9 on /Volumes/Other (vixenfs, fskit)"
        ));
    }
}
