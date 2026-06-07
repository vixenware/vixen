//! End-to-end VixenFS repair transaction.
//!
//! This is the app's single "fix it" path. It keeps the privileged work inside one
//! fixed management capability, while still removing stale per-login-session
//! PlugInKit state by re-entering the bundled helper as the console user.

use std::ffi::{CStr, CString};
use std::process::Command;
use std::time::Duration;

use eyre::{Result, WrapErr, bail};

use crate::{extension, killswitch, mount, repair_status};

#[derive(Debug)]
struct ConsoleUser {
    uid: u32,
    name: String,
}

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
    run_step(
        "user-registration",
        "Cleaning stale app records for the logged-in user",
        || repair_user_session_extension(),
    )?;
    run_step(
        "root-registration",
        "Cleaning stale root app records",
        || extension::repair_installed_extension(),
    )?;
    run_step(
        "extension-diagnostics",
        "Recording FSKit registration state",
        || {
            extension::log_installed_extension_diagnostics();
            Ok(())
        },
    )?;

    let first_mount = run_step("mount", "Mounting VixenFS", mount::establish);
    match first_mount {
        Ok(()) => Ok(()),
        Err(err) if is_stale_fskit_error(&format!("{err:#}")) => {
            tracing::warn!(
                error = %format!("{err:#}"),
                "mount failed with stale FSKit state; restarting fskitd and retrying"
            );
            run_step(
                "restart-fskit",
                "Restarting FSKit to clear stale macOS state",
                || killswitch::terminate_fskitd().map(|_| ()),
            )?;
            run_step(
                "wait-for-fskit",
                "Waiting for FSKit to restart",
                wait_for_fskit_restart,
            )?;
            run_step(
                "extension-diagnostics-after-restart",
                "Recording FSKit registration state after restart",
                || {
                    extension::log_installed_extension_diagnostics();
                    Ok(())
                },
            )?;
            run_step(
                "mount-after-restart",
                "Mounting VixenFS after FSKit restart",
                mount_in_fresh_helper,
            )
        }
        Err(err) => Err(err),
    }
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
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("fresh helper mount failed: {detail}");
    }
    Ok(())
}

fn repair_user_session_extension() -> Result<()> {
    let Some(user) = console_user()? else {
        tracing::warn!("no logged-in console user; skipping user-session extension repair");
        return Ok(());
    };
    let exe = std::env::current_exe().wrap_err("resolving current helper path")?;
    tracing::info!(
        uid = user.uid,
        user = user.name,
        helper = %exe.display(),
        "running user-session extension repair"
    );

    let output = Command::new("/bin/launchctl")
        .args(["asuser", &user.uid.to_string()])
        .arg("/usr/bin/sudo")
        .arg("-u")
        .arg(&user.name)
        .arg(&exe)
        .arg("repair-extension")
        .env(
            "VIXEN_MANAGEMENT_LOG",
            "info,vx_management=debug,vixen_management=debug",
        )
        .output()
        .wrap_err("running user-session repair-extension")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::info!(
        status = ?output.status.code(),
        stdout = %stdout.trim(),
        stderr = %stderr.trim(),
        "user-session extension repair finished"
    );
    if !output.status.success() {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("user-session repair-extension failed: {detail}");
    }
    Ok(())
}

fn console_user() -> Result<Option<ConsoleUser>> {
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

fn is_stale_fskit_error(message: &str) -> bool {
    message.contains("Module eu.bearcove.vixenfs is disabled")
        || message.contains("Unable to invoke task")
        || message.contains("Connection invalid")
}

#[cfg(test)]
mod tests {
    use super::is_stale_fskit_error;

    #[test]
    fn recognizes_stale_fskit_errors() {
        assert!(is_stale_fskit_error(
            "mount disk4 -> /Volumes/Vixen/vfs failed: Module eu.bearcove.vixenfs is disabled!\nmount: Unable to invoke task"
        ));
        assert!(is_stale_fskit_error(
            "/usr/bin/pluginkit failed: match: Connection invalid"
        ));
        assert!(!is_stale_fskit_error("hdiutil attach failed"));
    }
}
