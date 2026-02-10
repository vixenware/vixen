//! Seatbelt sandbox for compiler subprocesses (macOS).
//!
//! Wraps compiler invocations with `sandbox-exec` to enforce:
//! - No network access
//! - File writes restricted to the action's output directory
//! - Unrestricted file reads (VFS controls what files exist)

#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
use tokio::process::Command;

use crate::service_vfs::ActionContext;

/// The seatbelt profile (SBPL) used for all sandboxed subprocesses.
///
/// Security model:
/// - Base: inherits system.sb for process startup, dyld, system libraries
/// - Process exec: VFS_ROOT + /usr/bin (for linker tools - TODO: use hermetic Zig lld)
/// - File reads/maps: restricted to VFS_ROOT only
/// - File writes: WRITE_DIR (action output) + /private/var/folders (temp files)
/// - Network: denied entirely — actions must be hermetic.
///
/// TODO: Replace system linker with hermetic Zig lld + mount system libraries in VFS
#[cfg(target_os = "macos")]
const SEATBELT_PROFILE: &str = r#"(version 1)
(debug deny)
(deny default)
(import "system.sb")
(allow process-fork)
(allow process-exec (subpath (param "VFS_ROOT")))
(allow process-exec (subpath (param "VFS_ROOT_ALT")))
(allow process-exec (subpath "/usr/bin"))
(allow process-exec (subpath "/Library/Developer/CommandLineTools"))
(allow file-read* (subpath (param "VFS_ROOT")))
(allow file-read* (subpath (param "VFS_ROOT_ALT")))
(allow file-read* (subpath "/usr"))
(allow file-read* (subpath "/System"))
(allow file-read* (subpath "/Library"))
(allow file-read* (subpath "/private"))
(allow file-write* (subpath (param "WRITE_DIR")))
(allow file-write* (subpath (param "WRITE_DIR_ALT")))
(allow file-write* (subpath "/private/var/folders"))
(deny network*)"#;

// #[cfg(target_os = "macos")]
// const SEATBELT_PROFILE: &str = r#"
// (version 1)
// (allow default)
// "#;

// #[cfg(target_os = "macos")]
// pub fn sandboxed_command(program: &str, action: &ActionContext) -> Command {
//     // dummy version that does not sandbox, for testing
//     tracing::trace!(program, "sandboxed_command: no sandbox (dummy for testing)");
//     Command::new(program)
// }

/// Create a `Command` that runs `program` under the seatbelt sandbox.
///
/// On non-macOS platforms, returns `Command::new(program)` directly.
#[cfg(target_os = "macos")]
pub fn sandboxed_command(program: &str, action: &ActionContext) -> Command {
    tracing::trace!(
        program,
        vfs_root = %action.mount_base,
        write_dir = %action.out_path,
        profile_len = SEATBELT_PROFILE.len(),
        "sandboxed_command: wrapping with sandbox-exec"
    );
    tracing::trace!(profile = %SEATBELT_PROFILE, "seatbelt profile");

    // Check if the program binary exists on disk before we try to run it.
    // This helps distinguish "sandbox-exec not found" from "program not found".
    let program_exists = std::path::Path::new(program).exists();
    let program_is_file = std::path::Path::new(program)
        .metadata()
        .map(|m| m.is_file())
        .unwrap_or(false);
    let program_permissions = std::fs::metadata(program)
        .map(|m| format!("{:o}", m.permissions().mode()))
        .unwrap_or_else(|e| format!("error: {e}"));

    tracing::trace!(
        program,
        program_exists,
        program_is_file,
        program_permissions,
        "sandboxed_command: program binary check"
    );

    let sandbox_exec_exists = std::path::Path::new("/usr/bin/sandbox-exec").exists();
    tracing::trace!(
        sandbox_exec_exists,
        "sandboxed_command: sandbox-exec binary check"
    );

    let write_dir_exists = action.out_path.as_std_path().exists();
    tracing::trace!(
        write_dir = %action.out_path,
        write_dir_exists,
        "sandboxed_command: write_dir check"
    );

    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(SEATBELT_PROFILE);

    // Pass both /tmp and /private/tmp variants since macOS may resolve either way
    let write_dir = action.out_path.to_string();
    let write_dir_alt = write_dir.replace("/tmp", "/private/tmp");
    let vfs_root = action.mount_base.to_string();
    let vfs_root_alt = vfs_root.replace("/tmp", "/private/tmp");

    cmd.arg("-D").arg(format!("WRITE_DIR={}", write_dir));
    cmd.arg("-D")
        .arg(format!("WRITE_DIR_ALT={}", write_dir_alt));
    cmd.arg("-D").arg(format!("VFS_ROOT={}", vfs_root));
    cmd.arg("-D").arg(format!("VFS_ROOT_ALT={}", vfs_root_alt));
    cmd.arg(program);

    // Set all temp-related env vars to the action's output directory so rustc uses it
    // for temp files instead of the system temp dir (which the sandbox blocks)
    cmd.env("TMPDIR", action.out_path.as_str());
    cmd.env("TMP", action.out_path.as_str());
    cmd.env("TEMP", action.out_path.as_str());
    cmd.env("TEMPDIR", action.out_path.as_str());

    tracing::trace!(
        sandbox_exec = "/usr/bin/sandbox-exec",
        write_dir = %action.out_path,
        vfs_root = %action.mount_base,
        tmpdir = %action.out_path,
        program,
        "spawning sandboxed command"
    );

    cmd
}

#[cfg(not(target_os = "macos"))]
pub fn sandboxed_command(program: &str, _action: &ActionContext) -> Command {
    tracing::trace!(program, "sandboxed_command: no sandbox (not macOS)");
    Command::new(program)
}
