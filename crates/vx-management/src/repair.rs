//! End-to-end VixenFS repair transaction.
//!
//! This is the app's single "fix it" path. It first tries the direct mount path, then
//! falls back to the manual reset sequence only if macOS says the mount did not work.

use std::process::Command;
use std::time::Duration;

use eyre::{Result, WrapErr, bail};

use crate::{extension, killswitch, repair_status};

pub fn repair_vixenfs() -> Result<()> {
    tracing::info!("VixenFS repair transaction started");
    repair_status::running("start", "Starting VixenFS repair");
    let outcome = repair_vixenfs_inner();
    match &outcome {
        Ok(()) => {
            repair_status::finish_ok("VixenFS repair finished");
            tracing::info!("VixenFS repair transaction finished");
        }
        Err(err) => {
            let message = format!("{err:#}");
            repair_status::finish_failed(&message);
            tracing::error!(error = %message, "VixenFS repair transaction failed");
        }
    }
    outcome
}

fn repair_vixenfs_inner() -> Result<()> {
    let initial_mount = attempt_mount_step("mount", "Mounting VixenFS");
    if initial_mount.mounted() {
        return Ok(());
    }
    run_step("restart-fskit", "Restarting FSKit before mounting", || {
        killswitch::terminate_fskitd().map(|_| ())
    })?;
    run_step(
        "wait-for-fskit",
        "Waiting for FSKit to restart",
        wait_for_fskit_restart,
    )?;
    let after_restart = attempt_mount_step(
        "mount-after-fskit-restart",
        "Mounting VixenFS after FSKit restart",
    );
    if after_restart.mounted() {
        return Ok(());
    }

    let diagnostics = collect_recent_fskit_logs();
    let diagnostic_excerpt = match diagnostics {
        Ok(logs) => logs,
        Err(err) => {
            let detail = format!("{err:#}");
            tracing::warn!(error = %detail, "failed to collect recent FSKit logs");
            format!("FSKit log collection failed: {detail}")
        }
    };
    tracing::warn!(
        initial_mount_error = %initial_mount.detail(),
        after_restart_error = %after_restart.detail(),
        fskit_logs = %diagnostic_excerpt,
        "mount still failed after FSKit restart"
    );

    if !should_try_registration_cleanup(
        initial_mount.detail(),
        after_restart.detail(),
        &diagnostic_excerpt,
    ) {
        bail!(
            "mount failed after FSKit restart: {}\n\nRecent FSKit logs:\n{}",
            after_restart.detail(),
            diagnostic_excerpt
        );
    }

    run_step(
        "user-app-records",
        "FSKit logs point at registration state; cleaning stale app records for the logged-in user",
        repair_user_session_app_records,
    )?;
    run_step(
        "root-app-records",
        "Cleaning stale app records for root",
        extension::repair_installed_app_records,
    )?;
    run_step(
        "restart-fskit-after-registration-cleanup",
        "Restarting FSKit after registration cleanup",
        || killswitch::terminate_fskitd().map(|_| ()),
    )?;
    run_step(
        "wait-for-fskit-after-registration-cleanup",
        "Waiting for FSKit to restart",
        wait_for_fskit_restart,
    )?;
    run_step(
        "mount-after-registration-cleanup",
        "Mounting VixenFS after registration cleanup",
        mount_in_fresh_helper,
    )
}

#[derive(Debug)]
enum MountAttempt {
    Mounted,
    Failed(String),
}

impl MountAttempt {
    fn mounted(&self) -> bool {
        matches!(self, Self::Mounted)
    }

    fn detail(&self) -> &str {
        match self {
            Self::Mounted => "mounted",
            Self::Failed(detail) => detail,
        }
    }
}

fn attempt_mount_step(step: &str, message: &str) -> MountAttempt {
    tracing::info!(step, message, "repair mount attempt started");
    repair_status::running(step, message);
    let outcome = mount_in_fresh_helper();
    match outcome {
        Ok(()) => {
            repair_status::ok(step, "VixenFS mounted");
            tracing::info!(step, "repair mount attempt succeeded");
            MountAttempt::Mounted
        }
        Err(err) => {
            let detail = format!("{err:#}");
            tracing::warn!(
                step,
                error = %detail,
                "repair mount attempt failed"
            );
            repair_status::running(step, "Mount failed; trying the next repair step");
            MountAttempt::Failed(detail)
        }
    }
}

fn collect_recent_fskit_logs() -> Result<String> {
    repair_status::running("fskit-logs", "Checking recent FSKit logs");
    let predicate = r#"(process == "fskitd" OR process == "fskit_agent" OR process == "VixenFS") AND (eventMessage CONTAINS[c] "eu.bearcove.vixenfs" OR eventMessage CONTAINS[c] "vixenfs" OR eventMessage CONTAINS[c] "disabled" OR eventMessage CONTAINS[c] "module" OR eventMessage CONTAINS[c] "configureUserClient" OR eventMessage CONTAINS[c] "VixenFS.appex")"#;
    let output = Command::new("/usr/bin/log")
        .args([
            "show",
            "--style",
            "compact",
            "--last",
            "5m",
            "--info",
            "--debug",
            "--predicate",
            predicate,
        ])
        .output()
        .wrap_err("collecting recent FSKit logs")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::info!(
        status = ?output.status.code(),
        stdout_len = stdout.len(),
        stderr = %stderr.trim(),
        "recent FSKit log collection finished"
    );
    if !output.status.success() {
        let detail = child_failure_detail(&stdout, &stderr);
        bail!("log show failed: {detail}");
    }

    let excerpt = trim_log_excerpt(&stdout);
    repair_status::ok("fskit-logs", "Recent FSKit logs collected");
    Ok(excerpt)
}

fn should_try_registration_cleanup(
    initial_mount_error: &str,
    after_restart_error: &str,
    logs: &str,
) -> bool {
    let haystack = format!("{initial_mount_error}\n{after_restart_error}\n{logs}").to_lowercase();
    let registration_markers = [
        "module eu.bearcove.vixenfs is disabled",
        "vixenfs.appex",
        "/volumes/dmg",
        "deriveddata",
        "launchservices",
        "pluginkit",
        "configureuserclient",
        "registration",
    ];
    let should_cleanup = registration_markers
        .iter()
        .any(|marker| haystack.contains(marker));
    tracing::info!(
        should_cleanup,
        "evaluated whether persistent mount failure points at stale registration state"
    );
    should_cleanup
}

fn run_step<F>(step: &str, message: &str, body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    tracing::info!(step, message, "repair step started");
    repair_status::running(step, message);
    let outcome = body();
    match &outcome {
        Ok(()) => {
            repair_status::ok(step, message);
            tracing::info!(step, "repair step succeeded");
        }
        Err(err) => {
            let detail = format!("{err:#}");
            repair_status::failed(step, &detail);
            tracing::warn!(step, error = %detail, "repair step failed");
        }
    }
    outcome
}

fn wait_for_fskit_restart() -> Result<()> {
    std::thread::sleep(Duration::from_secs(3));
    Ok(())
}

fn mount_in_fresh_helper() -> Result<()> {
    let exe = std::env::current_exe().wrap_err("resolving current helper path")?;
    tracing::info!(
        helper = %exe.display(),
        "running mount retry in a fresh helper process"
    );
    let output = Command::new(&exe)
        .arg("mount")
        .env(
            "VIXEN_MANAGEMENT_LOG",
            "info,vx_management=debug,vixen_management=debug",
        )
        .output()
        .wrap_err("running fresh helper mount")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::info!(
        status = ?output.status.code(),
        stdout = %stdout.trim(),
        stderr = %stderr.trim(),
        "fresh helper mount finished"
    );
    if !output.status.success() {
        let detail = child_failure_detail(&stdout, &stderr);
        bail!("fresh helper mount failed: {detail}");
    }
    Ok(())
}

fn repair_user_session_app_records() -> Result<()> {
    let Some(user) = console_user()? else {
        tracing::warn!("no logged-in console user; skipping user-session app registration cleanup");
        return Ok(());
    };
    let exe = std::env::current_exe().wrap_err("resolving current helper path")?;
    tracing::info!(
        uid = user.uid,
        user = user.name,
        helper = %exe.display(),
        "running user-session Vixen app registration cleanup"
    );

    let output = Command::new("/bin/launchctl")
        .args(["asuser", &user.uid.to_string()])
        .arg("/usr/bin/sudo")
        .arg("-u")
        .arg(&user.name)
        .arg(&exe)
        .arg("repair-app-records")
        .env(
            "VIXEN_MANAGEMENT_LOG",
            "info,vx_management=debug,vixen_management=debug",
        )
        .output()
        .wrap_err("running user-session repair-app-records")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::info!(
        status = ?output.status.code(),
        stdout = %stdout.trim(),
        stderr = %stderr.trim(),
        "user-session Vixen app registration cleanup finished"
    );
    if !output.status.success() {
        let detail = child_failure_detail(&stdout, &stderr);
        bail!("user-session repair-app-records failed: {detail}");
    }
    Ok(())
}

#[derive(Debug)]
struct ConsoleUser {
    uid: u32,
    name: String,
}

fn console_user() -> Result<Option<ConsoleUser>> {
    use std::ffi::{CStr, CString};

    let path = CString::new("/dev/console").expect("static path has no NUL");
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::stat(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).wrap_err("stat /dev/console");
    }
    let stat = unsafe { stat.assume_init() };
    let uid = stat.st_uid;
    if uid == 0 {
        return Ok(None);
    }

    let passwd = unsafe { libc::getpwuid(uid) };
    if passwd.is_null() {
        return Ok(None);
    }
    let name_ptr = unsafe { (*passwd).pw_name };
    if name_ptr.is_null() {
        return Ok(None);
    }
    let name = unsafe { CStr::from_ptr(name_ptr) }
        .to_str()
        .wrap_err("console username is not UTF-8")?
        .to_string();
    if name == "root" || name == "loginwindow" {
        return Ok(None);
    }
    Ok(Some(ConsoleUser { uid, name }))
}

fn child_failure_detail(stdout: &str, stderr: &str) -> String {
    let raw = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if raw.is_empty() {
        return "command failed without output".to_string();
    }

    if let Some(error_start) = raw.rfind("\nError: ") {
        return trim_failure_detail(&raw[error_start + 1..]);
    }
    if raw.starts_with("Error: ") {
        return trim_failure_detail(raw);
    }

    let lines = raw
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let start = lines.len().saturating_sub(12);
    trim_failure_detail(&lines[start..].join("\n"))
}

fn trim_failure_detail(detail: &str) -> String {
    const MAX_CHARS: usize = 1_600;

    let detail = detail.trim();
    let mut chars = detail.chars();
    let mut trimmed = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        trimmed.push_str("...");
    }
    trimmed
}

fn trim_log_excerpt(logs: &str) -> String {
    const MAX_CHARS: usize = 3_200;

    let lines = logs
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let excerpt = lines
        .iter()
        .rev()
        .scan(0usize, |total, line| {
            let next_total = *total + line.len() + 1;
            if next_total > MAX_CHARS && *total > 0 {
                None
            } else {
                *total = next_total;
                Some(*line)
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    if excerpt.is_empty() {
        "No matching FSKit logs in the last 5 minutes".to_string()
    } else {
        excerpt
    }
}

#[cfg(test)]
mod tests {
    use super::child_failure_detail;

    #[test]
    fn child_failure_detail_prefers_error_block_over_traces() {
        let stderr = "\
2026-06-07T15:45:07Z INFO vixen_management: starting
2026-06-07T15:45:09Z DEBUG vx_management::extension: running lsregister
Error: /System/Library/.../lsregister failed: failed to scan VixenFS.appex: -10814

Location:
    crates/vx-management/src/extension.rs:267:9";

        assert_eq!(
            child_failure_detail("", stderr),
            "Error: /System/Library/.../lsregister failed: failed to scan VixenFS.appex: -10814\n\nLocation:\n    crates/vx-management/src/extension.rs:267:9"
        );
    }

    #[test]
    fn child_failure_detail_uses_log_tail_without_error_block() {
        let stderr = (0..16)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            child_failure_detail("", &stderr),
            (4..16)
                .map(|index| format!("line {index}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
