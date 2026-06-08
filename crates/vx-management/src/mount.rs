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
//! Both `hdiutil attach` and the final `/sbin/mount` run **as the logged-in user**, not
//! as root. The FSKit extension runs in the user's `fskit_agent`, so it must be able to
//! `open(2)` the `/dev/diskN` node — which means the *user* must attach the image (a
//! root-attached node is root-owned and the extension gets "Probing resource: Permission
//! denied"). With a user-owned device node, the mount needs no privilege at all. The one
//! thing that does need root — creating and chowning the `/Volumes/Vixen/vfs` mountpoint,
//! since `/Volumes` is root-owned — is a one-time first-run step handled elsewhere.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use eyre::{Result, WrapErr, bail};
use vx_vfs_types::MACOS_VFS_MOUNT_PARENT;

use crate::{killswitch, session};

/// Where the block volume mounts — a real `/Volumes` descendant, not an arbitrary path.
pub use vx_vfs_types::MACOS_VFS_MOUNT_POINT as MOUNT_POINT;
/// The FSKit filesystem type — matches the extension's `FSFileSystemType`.
pub const FS_TYPE: &str = "vixenfs";
/// User-relative directory holding the backing image (under `$HOME`).
pub const IMAGE_DIR_REL: &str = "Library/Application Support/Vixen";
/// Filename of the mock backing image.
pub const IMAGE_FILE_NAME: &str = "vixen-volume.img";
/// Size of the mock image in bytes. Created sparse (`set_len`), so it costs ~0 real
/// bytes; the volume's reported size comes from the extension's statfs, not from this.
pub const IMAGE_SIZE_BYTES: u64 = 100 * 1024 * 1024;
const SACRIFICIAL_UNMOUNT_DEADLINE: Duration = Duration::from_secs(2);

/// The mock backing image path: a **user-owned** file under `$HOME`. It must be owned by
/// the user running the mount so that `hdiutil attach` produces a user-owned device node
/// the FSKit extension can open. Never holds file data — data is served from the CAS.
pub fn image_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| eyre::eyre!("HOME is not set; cannot locate the VixenFS backing image"))?;
    Ok(Path::new(&home).join(IMAGE_DIR_REL).join(IMAGE_FILE_NAME))
}

/// The one and only privileged operation: create the mountpoint under root-owned
/// `/Volumes` and chown it (and its `/Volumes/Vixen` parent) to the logged-in console
/// user, so that user can later mount and — crucially — `umount` it without any
/// privilege. **Must run as root** (invoked once at first run via a single admin
/// prompt). Idempotent: re-running it just re-asserts ownership.
pub fn setup_mountpoint() -> Result<()> {
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        bail!("setup-mountpoint must run as root (it creates and chowns {MOUNT_POINT})");
    }
    let Some(user) = session::console_user()? else {
        bail!("no logged-in console user to own the VixenFS mountpoint");
    };
    tracing::info!(
        uid = user.uid,
        user = %user.name,
        parent = MACOS_VFS_MOUNT_PARENT,
        mount_point = MOUNT_POINT,
        "setting up VixenFS mountpoint as root"
    );
    std::fs::create_dir_all(MOUNT_POINT).wrap_err("creating VixenFS mountpoint")?;
    // chown both the parent and the mountpoint: umount permission keys off ownership of
    // the directory the volume is mounted on.
    chown_to(Path::new(MACOS_VFS_MOUNT_PARENT), user.uid)?;
    chown_to(Path::new(MOUNT_POINT), user.uid)?;
    tracing::info!(
        mount_point = MOUNT_POINT,
        uid = user.uid,
        "VixenFS mountpoint ready"
    );
    Ok(())
}

fn chown_to(path: &Path, uid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).wrap_err("path has NUL")?;
    // gid -1 leaves the group unchanged.
    if unsafe { libc::chown(c_path.as_ptr(), uid, u32::MAX) } != 0 {
        return Err(std::io::Error::last_os_error())
            .wrap_err_with(|| format!("chown {} to uid {uid}", path.display()));
    }
    Ok(())
}

/// Establish the mount if it isn't already up: provision the device, then mount it.
/// Idempotent — a healthy existing mount is a no-op. Runs **as the logged-in user** (no
/// root): the user-owned device node is what lets the FSKit extension open it.
pub fn establish() -> Result<()> {
    tracing::info!(mount_point = MOUNT_POINT, "establishing VixenFS mount");
    if is_mounted()? {
        tracing::info!("{MOUNT_POINT} already mounted");
        return Ok(());
    }
    recover_existing_state()?;
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
pub fn remount() -> Result<()> {
    tracing::info!(mount_point = MOUNT_POINT, "remount requested");
    recover_existing_state().wrap_err("recovering existing VixenFS state before remount")?;
    let device = provision_device()?;
    if let Err(err) = mount_volume(&device) {
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
    // A stale device left by a previous crashed run may fail to detach (EBUSY). That
    // shouldn't abort recovery entirely — log it and continue. The fresh hdiutil attach
    // and mount attempt may still succeed.
    if let Err(err) = detach_backing_devices() {
        tracing::warn!(error = %format!("{err:#}"), "failed to detach existing VixenFS backing devices; continuing anyway");
    } else {
        tracing::info!("existing VixenFS state recovery finished");
    }
    Ok(())
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
    let image = image_path()?;
    let image_str = image.to_string_lossy().into_owned();
    tracing::info!(image_path = %image_str, "provisioning Vixen backing device");
    ensure_image(&image)?;
    let out = Command::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-imagekey",
            "diskimage-class=CRawDiskImage",
            "-nomount",
            &image_str,
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
fn ensure_image(path: &Path) -> Result<()> {
    if path.exists() {
        tracing::info!(image_path = %path.display(), "mock backing image already exists");
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap_err("creating image support dir")?;
    }
    let file = std::fs::File::create(path).wrap_err("creating mock image file")?;
    file.set_len(IMAGE_SIZE_BYTES)
        .wrap_err("sizing mock image (sparse)")?;
    tracing::info!(
        image_path = %path.display(),
        size_bytes = IMAGE_SIZE_BYTES,
        "created sparse mock backing image"
    );
    Ok(())
}

/// Mount the block device as `vixenfs` at [`MOUNT_POINT`]. Runs as the logged-in user;
/// the mountpoint must already exist and be user-owned (created once at first run).
fn mount_volume(device: &str) -> Result<()> {
    let devnode = format!("/dev/{device}");
    tracing::info!(
        device,
        devnode,
        parent = MACOS_VFS_MOUNT_PARENT,
        mount_point = MOUNT_POINT,
        fs_type = FS_TYPE,
        "mounting VixenFS volume"
    );
    // The mountpoint lives under root-owned `/Volumes`, so it is created and chowned to
    // the user once at first run (the only privileged step). If it's missing, that
    // one-time setup hasn't run — say so instead of emitting a cryptic mount error.
    if !Path::new(MOUNT_POINT).is_dir() {
        bail!(
            "mountpoint {MOUNT_POINT} does not exist; first-run setup must create and chown it (mkdir -p {MOUNT_POINT} && chown <user> {MOUNT_POINT})"
        );
    }
    clean_mountpoint()?;
    let out = run_mount_command(&devnode)?;
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
    // Belt-and-suspenders: the mount command exited 0 but FSKit can silently fail to
    // initialize, leaving the volume absent from the mount table. Verify it's actually
    // there before reporting success.
    if !is_mounted()? {
        bail!(
            "mount {devnode} -> {MOUNT_POINT} exited 0 but volume is not in the mount table; VixenFS extension may have crashed on initialization"
        );
    }
    Ok(())
}

/// Empty the mountpoint directory before mounting. A VixenFS mountpoint is a *pure mount
/// target* — it must never hold real files. Stale entries left in the underlying
/// directory by earlier sessions (leftover `c-*.sock` connection sockets, `.DS_Store`,
/// container metadata plists) make FSKit's final mount step fail with "a file with the
/// same name already exists" (Cocoa `NSFileWriteFileExistsError`). Recovery has already
/// confirmed nothing is mounted here, so removing the contents is safe.
fn clean_mountpoint() -> Result<()> {
    let entries = match std::fs::read_dir(MOUNT_POINT) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).wrap_err_with(|| format!("reading mountpoint {MOUNT_POINT}")),
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed += 1,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed to remove stale mountpoint entry");
            }
        }
    }
    if removed > 0 {
        tracing::info!(
            removed,
            mount_point = MOUNT_POINT,
            "cleaned stale entries from mountpoint before mounting"
        );
    }
    Ok(())
}

fn run_mount_command(devnode: &str) -> Result<Output> {
    // Mount runs as the logged-in user with no privilege escalation. The device node was
    // attached by this same user (so it is user-owned and the FSKit extension can open
    // it), and the mountpoint is user-owned, so a plain `/sbin/mount` is all that's
    // needed — no session/bootstrap trampoline, no root. Running this as root instead
    // fails ("Unable to invoke task"): the root context cannot reach the user's FSKit.
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        tracing::warn!(
            devnode,
            "VixenFS mount is being run as root; it must run as the logged-in user and will likely fail"
        );
    }
    tracing::info!(
        euid,
        devnode,
        fs_type = FS_TYPE,
        mount_point = MOUNT_POINT,
        "running VixenFS mount as the logged-in user"
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
    let devices =
        parse_attached_image_devices(&String::from_utf8_lossy(&out.stdout), IMAGE_FILE_NAME);
    tracing::info!(
        ?devices,
        image_file = IMAGE_FILE_NAME,
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

/// Extract the whole-disk BSD names attached for our backing image. Matches by **file
/// name** (`vixen-volume.img`), not full path, so a device attached for a stale path
/// (e.g. an earlier root-owned `/Library/...` image) is still recognized as ours and
/// detached. The name is Vixen-specific, so this won't catch unrelated disk images.
fn parse_attached_image_devices(hdiutil_info_stdout: &str, image_file_name: &str) -> Vec<String> {
    let mut devices = Vec::new();
    for block in hdiutil_info_stdout.split("================================================") {
        if !block.lines().any(|line| {
            line.strip_prefix("image-path")
                .and_then(|line| line.split_once(':'))
                .is_some_and(|(_, path)| path.trim().rsplit('/').next() == Some(image_file_name))
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
image-path      : /Users/amos/Library/Application Support/Vixen/vixen-volume.img
image-alias     : /Users/amos/Library/Application Support/Vixen/vixen-volume.img
autodiskmount   : false
/dev/disk7
================================================
image-path      : /Users/amos/Library/Application Support/Vixen/vixen-volume.img
image-alias     : /Users/amos/Library/Application Support/Vixen/vixen-volume.img
/dev/disk8
/dev/disk8s1	41504653-0000-11AA-AA11-00306543ECAC
"#;

        assert_eq!(
            parse_attached_image_devices(info, IMAGE_FILE_NAME),
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
    fn mount_runs_as_user_with_plain_mount() {
        let source = include_str!("mount.rs");
        // The mount is a plain user-side `/sbin/mount` — no privilege trampoline.
        assert!(source.contains(&["/sbin", "/mount"].concat()));
        // No session-borrowing primitive, no sudo, no self-re-exec. The user-owned
        // device node is what makes the mount work, not any session/bootstrap juggling.
        assert!(!source.contains(&["launch", "ctl"].concat()));
        assert!(!source.contains(&["as", "user"].concat()));
        assert!(!source.contains(&["bs", "exec"].concat()));
        assert!(!source.contains(&["sudo", " -u"].concat()));
        assert!(!source.contains(&["current", "_exe"].concat()));
    }
}
