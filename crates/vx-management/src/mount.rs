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

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use eyre::{Result, WrapErr, bail};
use vx_vfs_types::MACOS_VFS_MOUNT_PARENT;

use crate::{killswitch, session};

/// Where the block volume mounts — a real `/Volumes` descendant, not an arbitrary path.
pub use vx_vfs_types::MACOS_VFS_MOUNT_POINT as MOUNT_POINT;
/// The FSKit filesystem type — matches the extension's `FSFileSystemType`.
pub const FS_TYPE: &str = "vixenfs";
/// The mock backing image: sparse, root-owned, and never holds file data.
pub const IMAGE_PATH: &str = "/Library/Application Support/Vixen/vixen-volume.img";
/// Size of the mock image in bytes. Created sparse (`set_len`), so it costs ~0 real
/// bytes; the volume's reported size comes from the extension's statfs, not from this.
pub const IMAGE_SIZE_BYTES: u64 = 100 * 1024 * 1024;
const SACRIFICIAL_UNMOUNT_DEADLINE: Duration = Duration::from_secs(2);

/// Establish the mount if it isn't already up: provision the device, then mount it.
/// Idempotent — a healthy existing mount is a no-op. **Requires root** (the mount).
///
/// `caller_pid` is the PID of the process that triggered this action. When present, the
/// final `/sbin/mount` runs in that process's Mach bootstrap via `launchctl bsexec`,
/// which places it in the login/GUI session where `fskit_agent` is running. When absent
/// (direct CLI invocation), `launchctl asuser` is used instead — that works because the
/// calling process is already in the user's bootstrap.
pub fn establish(caller_pid: Option<u32>) -> Result<()> {
    tracing::info!(mount_point = MOUNT_POINT, "establishing VixenFS mount");
    if is_mounted()? {
        tracing::info!("{MOUNT_POINT} already mounted");
        return Ok(());
    }
    recover_existing_state()?;
    let device = provision_device()?;
    if let Err(err) = mount_volume(&device, caller_pid) {
        if let Err(detach_err) = detach_device(&device) {
            tracing::warn!(device, error = %detach_err, "failed to detach backing device after mount failure");
        }
        return Err(err);
    }
    tracing::info!("mounted {device} at {MOUNT_POINT}");
    Ok(())
}

/// Recover from an active or wedged VixenFS mount, then detach any mock backing devices.
///
/// A normal unmount is the first choice, but it runs in a sacrificial child. Once FSKit
/// is wedged, the unmount caller can become part of the wedge; that must never be the
/// management daemon itself. Killing `fskitd` is the last-resort escalation when the
/// sacrificial unmount does not clear the mount.
pub fn unmount() -> Result<()> {
    tracing::info!(mount_point = MOUNT_POINT, "unmount requested");
    recover_existing_state()
}

/// Unwind any existing VixenFS state and then establish a fresh mount.
pub fn remount(caller_pid: Option<u32>) -> Result<()> {
    tracing::info!(mount_point = MOUNT_POINT, "remount requested");
    recover_existing_state().wrap_err("recovering existing VixenFS state before remount")?;
    let device = provision_device()?;
    if let Err(err) = mount_volume(&device, caller_pid) {
        if let Err(detach_err) = detach_device(&device) {
            tracing::warn!(device, error = %detach_err, "failed to detach backing device after remount failure");
        }
        return Err(err).wrap_err("mounting fresh VixenFS device during remount");
    }
    tracing::info!(device, mount_point = MOUNT_POINT, "remounted VixenFS");
    Ok(())
}

fn recover_existing_state() -> Result<()> {
    tracing::info!(
        mount_point = MOUNT_POINT,
        "recovering existing VixenFS state"
    );
    let unmount = try_sacrificial_unmount()?;
    tracing::info!(outcome = ?unmount, "sacrificial unmount attempt finished");
    if unmount.needs_fskitd_kill() || is_mounted()? {
        tracing::warn!(
            outcome = ?unmount,
            "sacrificial unmount did not clear VixenFS; terminating fskitd"
        );
        killswitch::terminate_fskitd()?;
    }
    if is_mounted()? {
        bail!("{MOUNT_POINT} is still mounted after terminating fskitd");
    }
    let result = detach_backing_devices();
    tracing::info!(
        ok = result.is_ok(),
        "existing VixenFS state recovery finished"
    );
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SacrificialUnmount {
    NotMounted,
    Succeeded,
    Failed,
    TimedOut,
}

impl SacrificialUnmount {
    fn needs_fskitd_kill(self) -> bool {
        matches!(
            self,
            SacrificialUnmount::Failed | SacrificialUnmount::TimedOut
        )
    }
}

fn try_sacrificial_unmount() -> Result<SacrificialUnmount> {
    if !is_mounted()? {
        tracing::info!(
            mount_point = MOUNT_POINT,
            "sacrificial unmount skipped; not mounted"
        );
        return Ok(SacrificialUnmount::NotMounted);
    }

    tracing::info!("{MOUNT_POINT} is mounted; spawning sacrificial umount child");
    let mut child = Command::new("/sbin/umount")
        .arg(MOUNT_POINT)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .wrap_err("spawning sacrificial umount")?;

    let deadline = Instant::now() + SACRIFICIAL_UNMOUNT_DEADLINE;
    loop {
        if let Some(status) = child.try_wait().wrap_err("polling sacrificial umount")? {
            if status.success() {
                tracing::info!("sacrificial umount cleared {MOUNT_POINT}");
                return Ok(SacrificialUnmount::Succeeded);
            }
            tracing::warn!(?status, "sacrificial umount exited unsuccessfully");
            return Ok(SacrificialUnmount::Failed);
        }

        if Instant::now() >= deadline {
            tracing::warn!(
                pid = child.id(),
                "sacrificial umount did not exit; leaving recovery to FSKit kill-switch"
            );
            let _ = child.kill();
            return Ok(SacrificialUnmount::TimedOut);
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Provision the mock raw block device: create the sparse image if absent, attach it
/// raw with no filesystem mounted, and return its whole-disk BSD name (e.g. `disk18`).
pub fn provision_device() -> Result<String> {
    tracing::info!(image_path = IMAGE_PATH, "provisioning Vixen backing device");
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
    tracing::info!(
        status = ?out.status.code(),
        stdout = %String::from_utf8_lossy(&out.stdout).trim(),
        stderr = %String::from_utf8_lossy(&out.stderr).trim(),
        "hdiutil attach finished"
    );
    if !out.status.success() {
        bail!(
            "hdiutil attach failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let device = parse_attached_device(&stdout)
        .ok_or_else(|| eyre::eyre!("no whole-disk /dev/disk node in hdiutil output: {stdout:?}"))?;
    tracing::info!(device, "provisioned Vixen backing device");
    Ok(device)
}

/// Create the sparse mock image if it doesn't exist yet. `set_len` extends the file
/// without allocating blocks (a hole on APFS) — exactly what `mkfile -n` does, with no
/// dependency on an external tool (`mkfile` isn't present at a fixed path on macOS 26).
fn ensure_image() -> Result<()> {
    let path = std::path::Path::new(IMAGE_PATH);
    if path.exists() {
        tracing::info!(image_path = IMAGE_PATH, "mock backing image already exists");
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap_err("creating image support dir")?;
    }
    let file = std::fs::File::create(path).wrap_err("creating mock image file")?;
    file.set_len(IMAGE_SIZE_BYTES)
        .wrap_err("sizing mock image (sparse)")?;
    tracing::info!(
        image_path = IMAGE_PATH,
        size_bytes = IMAGE_SIZE_BYTES,
        "created sparse mock backing image"
    );
    Ok(())
}

/// Mount the block device as `vixenfs` at [`MOUNT_POINT`]. **Requires root.**
fn mount_volume(device: &str, caller_pid: Option<u32>) -> Result<()> {
    let devnode = format!("/dev/{device}");
    // Keep `/Volumes/Vixen` itself plain APFS; FSKit owns only the nested child.
    tracing::info!(
        device,
        devnode,
        parent = MACOS_VFS_MOUNT_PARENT,
        mount_point = MOUNT_POINT,
        fs_type = FS_TYPE,
        "mounting VixenFS volume"
    );
    std::fs::create_dir_all(MACOS_VFS_MOUNT_PARENT).wrap_err("creating Vixen mount parent")?;
    std::fs::create_dir_all(MOUNT_POINT).wrap_err("creating Vixen mount point")?;
    let out = run_mount_command(&devnode, caller_pid)?;
    tracing::info!(
        device,
        devnode,
        mount_point = MOUNT_POINT,
        status = ?out.status.code(),
        stdout = %String::from_utf8_lossy(&out.stdout).trim(),
        stderr = %String::from_utf8_lossy(&out.stderr).trim(),
        "mount command finished"
    );
    if !out.status.success() {
        bail!(
            "mount {devnode} -> {MOUNT_POINT} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn run_mount_command(devnode: &str, caller_pid: Option<u32>) -> Result<Output> {
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        // When launched from the system daemon, `launchctl asuser <uid>` only reaches
        // the user's background bootstrap, not the login/GUI session where `fskit_agent`
        // runs. `bsexec <pid>` enters the exact bootstrap of the calling app, which IS
        // in the GUI session. When invoked directly via sudo (no caller_pid), `asuser`
        // still works because the calling process is already in the GUI session.
        if let Some(pid) = caller_pid {
            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
            if alive {
                tracing::info!(
                    pid,
                    devnode,
                    fs_type = FS_TYPE,
                    mount_point = MOUNT_POINT,
                    "running VixenFS mount via bsexec into caller bootstrap"
                );
                return Command::new("/bin/launchctl")
                    .arg("bsexec")
                    .arg(pid.to_string())
                    .arg("/sbin/mount")
                    .args(["-F", "-t", FS_TYPE, devnode, MOUNT_POINT])
                    .output()
                    .wrap_err("running launchctl bsexec mount");
            }
            tracing::warn!(
                pid,
                "caller PID is no longer alive; falling back to launchctl asuser"
            );
        } else {
            tracing::warn!(
                devnode,
                "no caller PID found in trigger queue; falling back to launchctl asuser"
            );
        }
        let Some(user) = session::console_user()? else {
            bail!("no logged-in console user for VixenFS mount");
        };
        let uid = user.uid.to_string();
        tracing::info!(
            uid = user.uid,
            user = %user.name,
            devnode,
            fs_type = FS_TYPE,
            mount_point = MOUNT_POINT,
            "running VixenFS mount in active console bootstrap"
        );
        return Command::new("/bin/launchctl")
            .arg("asuser")
            .arg(&uid)
            .arg("/sbin/mount")
            .args(["-F", "-t", FS_TYPE, devnode, MOUNT_POINT])
            .output()
            .wrap_err("running launchctl asuser mount");
    }

    tracing::warn!(
        euid,
        devnode,
        fs_type = FS_TYPE,
        mount_point = MOUNT_POINT,
        "running VixenFS mount without root privileges"
    );
    Command::new("/sbin/mount")
        .args(["-F", "-t", FS_TYPE, devnode, MOUNT_POINT])
        .output()
        .wrap_err("running mount")
}

/// Detach any raw disk-image devices currently attached for [`IMAGE_PATH`].
fn detach_backing_devices() -> Result<()> {
    let devices = attached_image_devices()?;
    tracing::info!(?devices, "detaching Vixen backing devices");
    for device in devices {
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
    tracing::info!(
        device,
        status = ?out.status.code(),
        stdout = %String::from_utf8_lossy(&out.stdout).trim(),
        stderr = %String::from_utf8_lossy(&out.stderr).trim(),
        "hdiutil detach finished"
    );
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
    tracing::info!(
        status = ?out.status.code(),
        stdout_len = out.stdout.len(),
        stderr = %String::from_utf8_lossy(&out.stderr).trim(),
        "hdiutil info finished"
    );
    if !out.status.success() {
        bail!(
            "hdiutil info failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let devices = parse_attached_image_devices(&String::from_utf8_lossy(&out.stdout), IMAGE_PATH);
    tracing::info!(
        ?devices,
        image_path = IMAGE_PATH,
        "parsed attached Vixen image devices"
    );
    Ok(devices)
}

/// Is the vixenfs volume currently mounted at [`MOUNT_POINT`]?
pub fn is_mounted() -> Result<bool> {
    let out = Command::new("/sbin/mount")
        .output()
        .wrap_err("running mount")?;
    let mounted = mount_table_has_volume(&String::from_utf8_lossy(&out.stdout));
    tracing::debug!(
        mounted,
        status = ?out.status.code(),
        stdout_len = out.stdout.len(),
        stderr = %String::from_utf8_lossy(&out.stderr).trim(),
        "checked mount table for VixenFS"
    );
    Ok(mounted)
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

    #[test]
    fn recovery_path_keeps_unmount_sacrificial() {
        let source = include_str!("mount.rs");
        assert!(source.contains("spawning sacrificial umount"));
        assert!(source.contains(".spawn()"));
        assert!(!source.contains(&["disk", "util"].concat()));
    }

    #[test]
    fn root_mount_path_uses_bsexec_for_launchd_and_asuser_for_cli() {
        let source = include_str!("mount.rs");
        assert!(source.contains(&["/bin", "/launchctl"].concat()));
        assert!(source.contains("bsexec"));
        assert!(source.contains(&["as", "user"].concat()));
        assert!(source.contains(&["/sbin", "/mount"].concat()));
        assert!(!source.contains(&["current", "_exe"].concat()));
        assert!(!source.contains(&["sudo", " -u"].concat()));
    }
}
