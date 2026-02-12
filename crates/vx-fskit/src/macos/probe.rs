use camino::Utf8PathBuf;
use tokio::process::Command;

use super::Log;

/// Result of probing a mount point.
pub(crate) struct ProbeResult {
    /// mkdir returned EBUSY — something is mounted here.
    pub mkdir_ebusy: bool,
    /// The mount path (or a candidate) appears in `df` output.
    pub in_df: bool,
    /// The VixenFS extension process is running.
    pub extension_running: bool,
    /// Result of reading health.json via /bin/cat.
    pub health: HealthProbe,
}

pub(crate) enum HealthProbe {
    /// Haven't attempted (e.g. nothing is mounted).
    NotAttempted,
    /// /bin/cat returned successfully, here's the raw JSON.
    Ok(String),
    /// /bin/cat timed out — mount is zombied.
    Timeout,
    /// /bin/cat failed (non-zero exit, or couldn't spawn).
    Failed(String),
}

impl ProbeResult {
    /// Nothing is mounted here — clean state.
    pub fn is_clean(&self) -> bool {
        !self.mkdir_ebusy && !self.in_df
    }

    /// Something is mounted and healthy (health.json says connected: true).
    pub fn is_healthy(&self) -> bool {
        matches!(&self.health, HealthProbe::Ok(json) if json.contains("\"connected\":true") || json.contains("\"connected\": true"))
    }

    /// Something is mounted (EBUSY or in df) but not healthy.
    pub fn is_broken_mount(&self) -> bool {
        (self.mkdir_ebusy || self.in_df) && !self.is_healthy()
    }
}

/// Probe the mount point. Runs safe probes first, then optionally the
/// sacrificial /bin/cat for health.json.
pub(crate) async fn probe(
    mount_path: &Utf8PathBuf,
    candidates: &[String],
    log: &mut Log,
    read_health: bool,
) -> ProbeResult {
    log.record(format!("probing mount point {mount_path}"));

    // 1. mkdir — returns EBUSY if mounted
    let mkdir_ebusy = probe_mkdir(mount_path, log);

    // 2. df — check for mount in output
    let in_df = probe_df(candidates, log).await;

    // 3. pgrep — is extension running?
    let extension_running = probe_pgrep(log).await;

    // 4. health.json via /bin/cat (only if something looks mounted)
    let health = if read_health && (mkdir_ebusy || in_df) {
        probe_health(mount_path, log).await
    } else {
        if !mkdir_ebusy && !in_df {
            log.record("nothing mounted, skipping health.json probe");
        }
        HealthProbe::NotAttempted
    };

    let result = ProbeResult {
        mkdir_ebusy,
        in_df,
        extension_running,
        health,
    };

    log.record(format!(
        "probe result: mkdir_ebusy={}, in_df={}, extension_running={}, health={}",
        result.mkdir_ebusy,
        result.in_df,
        result.extension_running,
        match &result.health {
            HealthProbe::NotAttempted => "not_attempted".to_string(),
            HealthProbe::Ok(json) => format!("ok({json})"),
            HealthProbe::Timeout => "timeout".to_string(),
            HealthProbe::Failed(e) => format!("failed({e})"),
        }
    ));

    result
}

/// Try to mkdir the mount path. Returns true if EBUSY.
fn probe_mkdir(mount_path: &Utf8PathBuf, log: &mut Log) -> bool {
    let path = std::ffi::CString::new(mount_path.as_str()).unwrap();
    let ret = unsafe { libc::mkdir(path.as_ptr(), 0o755) };
    if ret == 0 {
        // mkdir succeeded — nothing mounted, clean up the dir we just created
        unsafe { libc::rmdir(path.as_ptr()) };
        log.record("mkdir: ok (nothing mounted)");
        false
    } else {
        let errno = std::io::Error::last_os_error();
        if errno.raw_os_error() == Some(libc::EBUSY) {
            log.record("mkdir: EBUSY (something is mounted)");
            true
        } else if errno.raw_os_error() == Some(libc::EEXIST) {
            log.record("mkdir: EEXIST (directory exists, nothing mounted)");
            false
        } else {
            log.record(format!("mkdir: {errno}"));
            false
        }
    }
}

/// Check if any of the candidates appear in `df` output.
async fn probe_df(candidates: &[String], log: &mut Log) -> bool {
    let output = match Command::new("df").output().await {
        Ok(output) => output,
        Err(e) => {
            log.record(format!("df: failed to run: {e}"));
            return false;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for candidate in candidates {
        if stdout.lines().any(|line| line.contains(candidate.as_str())) {
            log.record(format!("df: found mount matching {candidate}"));
            return true;
        }
    }
    log.record("df: no vixenfs mount found");
    false
}

/// Check if the VixenFS extension process is running.
async fn probe_pgrep(log: &mut Log) -> bool {
    let output = match Command::new("pgrep").args(["-x", "VixenFS"]).output().await {
        Ok(output) => output,
        Err(e) => {
            log.record(format!("pgrep: failed to run: {e}"));
            return false;
        }
    };

    let running = output.status.success() && !output.stdout.is_empty();
    if running {
        let pids = String::from_utf8_lossy(&output.stdout);
        log.record(format!("pgrep: VixenFS running (pids: {})", pids.trim()));
    } else {
        log.record("pgrep: VixenFS not running");
    }
    running
}

/// Read health.json via a sacrificial /bin/cat process with timeout.
async fn probe_health(mount_path: &Utf8PathBuf, log: &mut Log) -> HealthProbe {
    let health_path = format!("{mount_path}/.vixenfs/health.json");
    log.record(format!("reading {health_path} via /bin/cat"));

    let mut child = match Command::new("/bin/cat")
        .arg(&health_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            log.record(format!("/bin/cat: failed to spawn: {e}"));
            return HealthProbe::Failed(e.to_string());
        }
    };

    match tokio::time::timeout(std::time::Duration::from_secs(1), child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let json = String::from_utf8_lossy(&output.stdout).to_string();
            log.record(format!("health.json: {json}"));
            HealthProbe::Ok(json)
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log.record(format!(
                "health.json: cat failed (exit={}): {}",
                output.status,
                stderr.trim()
            ));
            HealthProbe::Failed(stderr.trim().to_string())
        }
        Ok(Err(e)) => {
            log.record(format!("health.json: cat error: {e}"));
            HealthProbe::Failed(e.to_string())
        }
        Err(_) => {
            // Timed out
            log.record("health.json: /bin/cat timed out (mount is zombied), killing");
            HealthProbe::Timeout
        }
    }
}
