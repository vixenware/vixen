//! The wedge kill-switch: terminate the system `fskitd`.
//!
//! When `fskitd` itself wedges (holding stale FSKit module state or a mount no
//! ordinary unmount can clear), the practical fix is to terminate the daemon process.
//! Do not ask launchd to do it: this path intentionally resolves `/usr/libexec/fskitd`
//! and calls `kill(2)` on that PID directly.

use std::time::{Duration, Instant};

use eyre::{Result, WrapErr, bail};

const FSKITD_PATH: &str = "/usr/libexec/fskitd";
const PROC_ALL_PIDS: u32 = 1;

/// Terminate `fskitd`: `SIGTERM`, escalating to `SIGKILL` if it doesn't exit within a
/// bounded wait. Returns whether a process was found and signalled.
pub fn terminate_fskitd() -> Result<bool> {
    let pids = fskitd_pids()?;
    if pids.is_empty() {
        tracing::info!("fskitd not running; nothing to terminate");
        return Ok(false);
    }

    tracing::warn!(?pids, "terminating fskitd with kill(2) SIGTERM");
    signal_pids(&pids, libc::SIGTERM)?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let remaining = live_pids(&pids);
        if remaining.is_empty() {
            tracing::info!(?pids, "fskitd exited after SIGTERM");
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let remaining = live_pids(&pids);
    if !remaining.is_empty() {
        tracing::warn!(
            ?remaining,
            "fskitd survived SIGTERM; escalating to kill(2) SIGKILL"
        );
        signal_pids(&remaining, libc::SIGKILL)?;
    }
    Ok(true)
}

fn signal_pids(pids: &[libc::pid_t], signal: libc::c_int) -> Result<()> {
    for &pid in pids {
        signal_pid(pid, signal)?;
    }
    Ok(())
}

fn signal_pid(pid: libc::pid_t, signal: libc::c_int) -> Result<()> {
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }

    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).wrap_err_with(|| format!("kill({pid}, {signal})"))
}

fn live_pids(pids: &[libc::pid_t]) -> Vec<libc::pid_t> {
    pids.iter()
        .copied()
        .filter(|&pid| unsafe { libc::kill(pid, 0) } == 0)
        .collect()
}

fn fskitd_pids() -> Result<Vec<libc::pid_t>> {
    let pids = all_pids()?;
    let mut matches = Vec::new();
    for pid in pids {
        if let Some(path) = pid_path(pid)
            && path_matches_fskitd(&path)
        {
            matches.push(pid);
        }
    }
    Ok(matches)
}

fn all_pids() -> Result<Vec<libc::pid_t>> {
    let byte_count = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if byte_count < 0 {
        return Err(std::io::Error::last_os_error()).wrap_err("proc_listpids size query");
    }

    let mut capacity = (byte_count as usize / std::mem::size_of::<libc::pid_t>()).max(64) + 64;
    loop {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let buffer_size = (pids.len() * std::mem::size_of::<libc::pid_t>())
            .try_into()
            .wrap_err("pid buffer too large")?;
        let written =
            unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, pids.as_mut_ptr().cast(), buffer_size) };
        if written < 0 {
            return Err(std::io::Error::last_os_error()).wrap_err("proc_listpids");
        }

        let count = written as usize / std::mem::size_of::<libc::pid_t>();
        if count < pids.len() {
            pids.truncate(count);
            pids.retain(|pid| *pid > 0);
            return Ok(pids);
        }

        capacity *= 2;
        if capacity > 1_000_000 {
            bail!("proc_listpids result kept growing past {capacity} entries");
        }
    }
}

fn pid_path(pid: libc::pid_t) -> Option<String> {
    let mut buffer = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let written = unsafe {
        libc::proc_pidpath(
            pid,
            buffer.as_mut_ptr().cast(),
            libc::PROC_PIDPATHINFO_MAXSIZE as u32,
        )
    };
    if written <= 0 {
        return None;
    }

    let max_len = (written as usize).min(buffer.len());
    let len = buffer[..max_len]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(max_len);
    Some(String::from_utf8_lossy(&buffer[..len]).into_owned())
}

fn path_matches_fskitd(path: &str) -> bool {
    path == FSKITD_PATH
}

#[cfg(test)]
mod tests {
    use super::path_matches_fskitd;

    #[test]
    fn matches_exact_fskitd_path() {
        assert!(path_matches_fskitd("/usr/libexec/fskitd"));
        assert!(!path_matches_fskitd("/usr/libexec/fskit_agent"));
        assert!(!path_matches_fskitd("/tmp/fskitd"));
    }

    #[test]
    fn kill_switch_uses_direct_pid_signals() {
        let source = include_str!("killswitch.rs");
        let forbidden = [
            ["launch", "ctl"].concat(),
            concat!("pg", "rep").to_string(),
            concat!("kill", "all").to_string(),
            ["/bin/", "ps"].concat(),
            ["/usr/bin/", "ps"].concat(),
        ];
        for forbidden in forbidden {
            assert!(
                !source.contains(&forbidden),
                "kill-switch must not use `{forbidden}`"
            );
        }
    }
}
