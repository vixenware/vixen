//! Establishing the block-backed `vixenfs` volume.
//!
//! macOS 15+ supports only block-device FSKit resources, so the CAS-VFS is mounted on
//! a **mock raw disk image**: a sparse file attached as a raw block device whose bytes
//! are never read. File data is still served by the extension from the CAS over Vox —
//! the device is only what FSKit binds the extension through. Being a block volume, it
//! mounts under `/Volumes` as a *local* volume, free of the generic-URL TCC gate that
//! forced Full Disk Access before. The actual mountpoint is intentionally nested below
//! a plain `/Volumes` child so a wedged FSKit root does not poison `/Volumes` itself.
//!
//! `mount(2)` needs root, so these run inside the privileged daemon. Provisioning the
//! image + `hdiutil attach` is unprivileged, but the daemon owns the whole sequence so
//! its trigger stays payload-less (no caller names the device or path).

use std::process::Command;

use eyre::{Result, WrapErr, bail};
use vx_vfs_types::MACOS_VFS_MOUNT_PARENT;

/// Where the block volume mounts — a real `/Volumes` descendant, not an arbitrary path.
pub use vx_vfs_types::MACOS_VFS_MOUNT_POINT as MOUNT_POINT;
/// The FSKit filesystem type — matches the extension's `FSFileSystemType`.
pub const FS_TYPE: &str = "vixenfs";
/// The mock backing image: sparse, root-owned, and never holds file data.
pub const IMAGE_PATH: &str = "/Library/Application Support/Vixen/vixen-volume.img";
/// Size of the mock image in bytes. Created sparse (`set_len`), so it costs ~0 real
/// bytes; the volume's reported size comes from the extension's statfs, not from this.
pub const IMAGE_SIZE_BYTES: u64 = 100 * 1024 * 1024;

/// Establish the mount if it isn't already up: provision the device, then mount it.
/// Idempotent — a healthy existing mount is a no-op. **Requires root** (the mount).
pub fn establish() -> Result<()> {
    if is_mounted()? {
        tracing::info!("{MOUNT_POINT} already mounted");
        return Ok(());
    }
    detach_backing_devices()?;
    let device = provision_device()?;
    if let Err(err) = mount_volume(&device) {
        if let Err(detach_err) = detach_device(&device) {
            tracing::warn!(device, error = %detach_err, "failed to detach backing device after mount failure");
        }
        return Err(err);
    }
    tracing::info!("mounted {device} at {MOUNT_POINT}");
    Ok(())
}

/// Unmount the volume if it is up, then detach any Vixen mock backing devices.
/// Idempotent. **Requires root.**
pub fn unmount() -> Result<()> {
    if is_mounted()? {
        let out = Command::new("/sbin/umount")
            .arg(MOUNT_POINT)
            .output()
            .wrap_err("running umount")?;
        if !out.status.success() {
            bail!(
                "unmount {MOUNT_POINT} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        tracing::info!("unmounted {MOUNT_POINT}");
    }
    detach_backing_devices()
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

/// Create the sparse mock image if it doesn't exist yet. `set_len` extends the file
/// without allocating blocks (a hole on APFS) — exactly what `mkfile -n` does, with no
/// dependency on an external tool (`mkfile` isn't present at a fixed path on macOS 26).
fn ensure_image() -> Result<()> {
    let path = std::path::Path::new(IMAGE_PATH);
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap_err("creating image support dir")?;
    }
    let file = std::fs::File::create(path).wrap_err("creating mock image file")?;
    file.set_len(IMAGE_SIZE_BYTES)
        .wrap_err("sizing mock image (sparse)")?;
    Ok(())
}

/// Mount the block device as `vixenfs` at [`MOUNT_POINT`]. **Requires root.**
fn mount_volume(device: &str) -> Result<()> {
    // Keep `/Volumes/Vixen` itself plain APFS; FSKit owns only the nested child.
    std::fs::create_dir_all(MACOS_VFS_MOUNT_PARENT).ok();
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

/// Detach any raw disk-image devices currently attached for [`IMAGE_PATH`].
fn detach_backing_devices() -> Result<()> {
    for device in attached_image_devices()? {
        detach_device(&device)?;
    }
    Ok(())
}

fn detach_device(device: &str) -> Result<()> {
    let devnode = format!("/dev/{device}");
    let out = Command::new("/usr/bin/hdiutil")
        .args(["detach", &devnode])
        .output()
        .wrap_err_with(|| format!("running hdiutil detach {devnode}"))?;
    if !out.status.success() {
        bail!(
            "hdiutil detach {devnode} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    tracing::info!(device, "detached Vixen backing image");
    Ok(())
}

fn attached_image_devices() -> Result<Vec<String>> {
    let out = Command::new("/usr/bin/hdiutil")
        .arg("info")
        .output()
        .wrap_err("running hdiutil info")?;
    if !out.status.success() {
        bail!(
            "hdiutil info failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_attached_image_devices(
        &String::from_utf8_lossy(&out.stdout),
        IMAGE_PATH,
    ))
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

/// Extract the whole-disk BSD names attached for a specific `hdiutil info` image block.
fn parse_attached_image_devices(hdiutil_info_stdout: &str, image_path: &str) -> Vec<String> {
    let mut devices = Vec::new();
    for block in hdiutil_info_stdout.split("================================================") {
        if !block.lines().any(|line| {
            line.strip_prefix("image-path")
                .and_then(|line| line.split_once(':'))
                .is_some_and(|(_, path)| path.trim() == image_path)
        }) {
            continue;
        }
        for line in block.lines() {
            if let Some(device) = parse_whole_disk_line(line) {
                devices.push(device);
            }
        }
    }
    devices
}

/// Extract the whole-disk BSD name (`disk18`) from `hdiutil attach` output, skipping
/// partition slices (`disk18s1`).
fn parse_attached_device(hdiutil_stdout: &str) -> Option<String> {
    for line in hdiutil_stdout.lines() {
        if let Some(device) = parse_whole_disk_line(line) {
            return Some(device);
        }
    }
    None
}

fn parse_whole_disk_line(line: &str) -> Option<String> {
    let tok = line.split_whitespace().next()?;
    let name = tok.strip_prefix("/dev/")?;
    let num = name.strip_prefix("disk")?;
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_string())
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
    fn parses_attached_devices_for_vixen_image_only() {
        let info = r#"
framework       : 683.100.3
================================================
image-path      : /System/Library/AssetsV2/toolchain.dmg
image-alias     : /System/Library/AssetsV2/toolchain.dmg
/dev/disk5	GUID_partition_scheme
/dev/disk5s1	7C3457EF-0000-11AA-AA11-00306543ECAC
================================================
image-path      : /Library/Application Support/Vixen/vixen-volume.img
image-alias     : /Library/Application Support/Vixen/vixen-volume.img
autodiskmount   : false
/dev/disk7
================================================
image-path      : /Library/Application Support/Vixen/vixen-volume.img
image-alias     : /Library/Application Support/Vixen/vixen-volume.img
/dev/disk8
/dev/disk8s1	41504653-0000-11AA-AA11-00306543ECAC
"#;

        assert_eq!(
            parse_attached_image_devices(info, IMAGE_PATH),
            vec!["disk7".to_string(), "disk8".to_string()]
        );
    }

    #[test]
    fn detects_volume_in_mount_table() {
        assert!(mount_table_has_volume(
            "disk18 on /Volumes/Vixen/vfs (vixenfs, nodev, nosuid, fskit, mounted by amos)"
        ));
        assert!(!mount_table_has_volume(
            "/dev/disk1s1 on / (apfs, local, journaled)"
        ));
        // Right fs, wrong mountpoint.
        assert!(!mount_table_has_volume(
            "disk9 on /Volumes/Other (vixenfs, fskit)"
        ));
    }

    #[test]
    fn mount_point_is_nested_below_volumes_parent() {
        assert_eq!(MOUNT_POINT, format!("{MACOS_VFS_MOUNT_PARENT}/vfs"));
        assert_ne!(MOUNT_POINT, MACOS_VFS_MOUNT_PARENT);
    }
}
