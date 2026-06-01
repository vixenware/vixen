//! The wedge kill-switch: terminate the system `fskitd`.
//!
//! When `fskitd` itself wedges (holding a mount no unprivileged unmount can clear),
//! the only fix is to terminate it. It is launch-on-demand with **no `KeepAlive`**, so
//! killing it is recoverable by design — it stays down until the next mount demand
//! relaunches it cleanly. The target is resolved **here**, never from caller input.

use std::time::{Duration, Instant};

use eyre::{Result, bail};

/// The launchd job's executable basename — `/usr/libexec/fskitd`.
const FSKITD: &str = "fskitd";

/// Resolve the running `fskitd` PID, if any. Resolved by the daemon itself — a caller
/// never names the target.
pub fn resolve_fskitd_pid() -> Option<u32> {
    let out = std::process::Command::new("/usr/bin/pgrep")
        .args(["-x", FSKITD])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Terminate `fskitd`: `SIGTERM`, escalating to `SIGKILL` if it doesn't exit within a
/// bounded wait. Returns whether a process was found and signalled. At most one
/// terminate per call.
pub fn terminate_fskitd() -> Result<bool> {
    let Some(pid) = resolve_fskitd_pid() else {
        tracing::info!("fskitd not running; nothing to terminate");
        return Ok(false);
    };
    tracing::warn!("terminating fskitd pid={pid} (SIGTERM)");
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(true); // already gone between resolve and signal
        }
        bail!("SIGTERM fskitd pid={pid}: {err}");
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            tracing::info!("fskitd pid={pid} exited after SIGTERM");
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::warn!("fskitd pid={pid} survived SIGTERM; escalating to SIGKILL");
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    Ok(true)
}

/// `kill(pid, 0)`: succeeds while the process is alive, `ESRCH` once it is gone.
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
