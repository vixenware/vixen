//! The wedge kill-switch: terminate the system `fskitd`.
//!
//! When `fskitd` itself wedges (holding a mount no unprivileged unmount can clear),
//! the only fix is to terminate it. It is launch-on-demand with **no `KeepAlive`**, so
//! killing it is recoverable by design — it stays down until the next mount demand
//! relaunches it cleanly. The target is resolved **here**, never from caller input.

use std::process::Command;
use std::time::{Duration, Instant};

use eyre::{Result, WrapErr, bail};

/// The launchd service target for `/usr/libexec/fskitd`.
const FSKITD_SERVICE: &str = "system/com.apple.filesystems.fskitd";

/// The non-enumerating control program for launchd jobs.
const LAUNCHCTL: &str = "/bin/launchctl";

/// Terminate `fskitd`: `SIGTERM`, escalating to `SIGKILL` if it doesn't exit within a
/// bounded wait. Returns whether a process was found and signalled. At most one
/// terminate per call.
pub fn terminate_fskitd() -> Result<bool> {
    if !fskitd_service_running()? {
        tracing::info!("fskitd not running; nothing to terminate");
        return Ok(false);
    }

    tracing::warn!("terminating fskitd service={FSKITD_SERVICE} (SIGTERM)");
    signal_fskitd("SIGTERM")?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if !fskitd_service_running()? {
            tracing::info!("fskitd service={FSKITD_SERVICE} exited after SIGTERM");
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    tracing::warn!("fskitd service={FSKITD_SERVICE} survived SIGTERM; escalating to SIGKILL");
    signal_fskitd("SIGKILL")?;
    Ok(true)
}

fn signal_fskitd(signal: &str) -> Result<()> {
    let output = Command::new(LAUNCHCTL)
        .args(["kill", signal, FSKITD_SERVICE])
        .output()
        .wrap_err_with(|| format!("running {LAUNCHCTL} kill {signal} {FSKITD_SERVICE}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "{LAUNCHCTL} kill {signal} {FSKITD_SERVICE} failed: {}",
        stderr.trim()
    );
}

fn fskitd_service_running() -> Result<bool> {
    let output = Command::new(LAUNCHCTL)
        .args(["print", FSKITD_SERVICE])
        .output()
        .wrap_err_with(|| format!("running {LAUNCHCTL} print {FSKITD_SERVICE}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not find service") || stderr.contains("No such process") {
            return Ok(false);
        }
        bail!(
            "{LAUNCHCTL} print {FSKITD_SERVICE} failed: {}",
            stderr.trim()
        );
    }

    Ok(launchctl_print_reports_running(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn launchctl_print_reports_running(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == "state = running" || line.trim() == "job state = running")
}

#[cfg(test)]
mod tests {
    use super::launchctl_print_reports_running;

    #[test]
    fn detects_running_launchd_service() {
        assert!(launchctl_print_reports_running(
            "system/com.apple.filesystems.fskitd = {\n\tstate = running\n\tpid = 411\n}\n"
        ));
        assert!(launchctl_print_reports_running(
            "system/com.apple.filesystems.fskitd = {\n\tjob state = running\n}\n"
        ));
    }

    #[test]
    fn non_running_launchd_service_is_not_running() {
        assert!(!launchctl_print_reports_running(
            "system/com.apple.filesystems.fskitd = {\n\tstate = waiting\n}\n"
        ));
        assert!(!launchctl_print_reports_running(""));
    }

    #[test]
    fn kill_switch_stays_launchd_targeted() {
        let source = include_str!("killswitch.rs");
        assert!(source.contains("system/com.apple.filesystems.fskitd"));
        let forbidden = [
            concat!("pg", "rep").to_string(),
            concat!("kill", "all").to_string(),
            ["/bin/", "ps"].concat(),
            ["/usr/bin/", "ps"].concat(),
        ];
        for forbidden in forbidden {
            assert!(
                !source.contains(&forbidden),
                "kill-switch must not use process-name lookup tool `{forbidden}`"
            );
        }
    }
}
