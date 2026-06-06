//! The trigger queue — launchd `QueueDirectories`.
//!
//! A file appearing in a button's queue directory *is* the request: launchd starts the
//! daemon, which runs the button and then **drains** the queue (unlinks the trigger
//! files) so launchd stops relaunching it. Authorization is by **filesystem permission
//! on the queue directory** (`root:admin`, `1730`), not by authenticating the caller —
//! any admin may drop a trigger, but cannot read or clear another's. The narrowness of
//! each button is the security property; the queue need only gate *who may fire it*.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use eyre::{Result, WrapErr};

use crate::Button;

/// Process-local counter for unique trigger filenames.
static TRIGGER_SEQ: AtomicU64 = AtomicU64::new(0);

/// Root-owned support directory for the management daemon.
pub const SUPPORT_DIR: &str = "/Library/Application Support/Vixen/management";

/// The queue directory for a button.
pub fn queue_dir(button: Button) -> PathBuf {
    Path::new(SUPPORT_DIR)
        .join("queue")
        .join(button.queue_name())
}

/// Does this button's queue hold at least one trigger file?
pub fn is_fired(button: Button) -> bool {
    let dir = queue_dir(button);
    match fs::read_dir(&dir) {
        Ok(mut entries) => {
            let fired = entries.any(|e| e.is_ok());
            tracing::debug!(button = ?button, dir = %dir.display(), fired, "checked trigger queue");
            fired
        }
        Err(err) => {
            tracing::debug!(button = ?button, dir = %dir.display(), error = %err, "could not read trigger queue");
            false
        }
    }
}

/// Fire a button by dropping a uniquely-named trigger file into its queue. This is the
/// **unprivileged** side (any admin may do it); launchd then starts the daemon. Returns
/// the trigger path so the caller can ack-by-drain — watch for its removal as the signal
/// that the daemon ran. Fails if the queue directory is absent (the daemon isn't
/// installed), which is itself the "management daemon unavailable" signal.
pub fn fire(button: Button) -> Result<PathBuf> {
    let dir = queue_dir(button);
    let trigger = dir.join(format!(
        "trigger-{}-{}",
        std::process::id(),
        TRIGGER_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&trigger, b"").wrap_err_with(|| {
        format!("dropping trigger in {dir:?} (is Vixen Management installed?)")
    })?;
    tracing::info!(button = ?button, trigger = %trigger.display(), "dropped management trigger");
    Ok(trigger)
}

/// Has a fired trigger been consumed (removed) by the daemon? The ack-by-drain signal.
pub fn is_consumed(trigger: &Path) -> bool {
    let consumed = !trigger.exists();
    tracing::debug!(trigger = %trigger.display(), consumed, "checked management trigger consumption");
    consumed
}

/// Remove all trigger files from a button's queue (required so launchd stops
/// relaunching). Idempotent — a missing queue is fine.
pub fn drain(button: Button) -> Result<()> {
    let dir = queue_dir(button);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(button = ?button, dir = %dir.display(), "queue missing while draining");
            return Ok(());
        }
        Err(e) => return Err(e).wrap_err_with(|| format!("reading queue {dir:?}")),
    };
    let mut drained = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        match fs::remove_file(&path) {
            Ok(()) => {
                drained += 1;
                tracing::info!(button = ?button, trigger = %path.display(), "drained management trigger");
            }
            Err(err) => {
                tracing::warn!(button = ?button, trigger = %path.display(), error = %err, "failed to drain management trigger");
            }
        }
    }
    tracing::info!(button = ?button, dir = %dir.display(), drained, "finished draining queue");
    Ok(())
}

/// Create the support + per-button queue directories and assert their ownership/mode
/// (`root:admin`, `1730` on each queue). Called on `RunAtLoad`; repairs perms if wrong,
/// so a misconfigured queue never silently widens who may press a button.
pub fn ensure_queues() -> Result<()> {
    let queue_root = Path::new(SUPPORT_DIR).join("queue");
    tracing::info!(queue_root = %queue_root.display(), "ensuring management queues");
    fs::create_dir_all(&queue_root).wrap_err("creating queue root")?;
    let gid = admin_gid();
    for button in Button::ALL {
        let dir = queue_dir(button);
        fs::create_dir_all(&dir).wrap_err_with(|| format!("creating {dir:?}"))?;
        set_owner_mode(&dir, 0, gid, 0o1730).wrap_err_with(|| format!("hardening {dir:?}"))?;
        tracing::info!(
            button = ?button,
            dir = %dir.display(),
            uid = 0,
            gid,
            mode = "1730",
            "management queue ready"
        );
    }
    Ok(())
}

/// gid of the `admin` group — resolved rather than hardcoded (80 on stock macOS).
fn admin_gid() -> u32 {
    let name = match std::ffi::CString::new("admin") {
        Ok(name) => name,
        Err(_) => return 80,
    };
    let grp = unsafe { libc::getgrnam(name.as_ptr()) };
    if grp.is_null() {
        80
    } else {
        unsafe { (*grp).gr_gid }
    }
}

fn set_owner_mode(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).wrap_err("path has NUL")?;
    if unsafe { libc::chown(c_path.as_ptr(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error()).wrap_err("chown");
    }
    if unsafe { libc::chmod(c_path.as_ptr(), mode as libc::mode_t) } != 0 {
        return Err(std::io::Error::last_os_error()).wrap_err("chmod");
    }
    Ok(())
}
