//! Running privileged helper commands in the active login session.

use std::ffi::{CStr, CString};
use std::process::Command;

use eyre::{Result, WrapErr, bail};

pub fn run_current_helper_in_console_bootstrap(command: &str) -> Result<()> {
    let Some(user) = console_user()? else {
        bail!("no logged-in console user for FSKit bootstrap");
    };
    let exe = std::env::current_exe().wrap_err("resolving current helper path")?;
    tracing::info!(
        uid = user.uid,
        user = user.name,
        helper = %exe.display(),
        command,
        "running helper command in console user bootstrap"
    );

    let output = Command::new("/bin/launchctl")
        .args(["asuser", &user.uid.to_string()])
        .arg(&exe)
        .arg(command)
        .env(
            "VIXEN_MANAGEMENT_LOG",
            "info,vx_management=debug,vixen_management=debug",
        )
        .output()
        .wrap_err("running helper command in console user bootstrap")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::info!(
        uid = user.uid,
        user = user.name,
        command,
        status = ?output.status.code(),
        stdout = %stdout.trim(),
        stderr = %stderr.trim(),
        "console bootstrap helper command finished"
    );
    if !output.status.success() {
        bail!(
            "console bootstrap {command} failed: {}",
            child_failure_detail(&stdout, &stderr)
        );
    }
    Ok(())
}

#[derive(Debug)]
struct ConsoleUser {
    uid: u32,
    name: String,
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

fn child_failure_detail(stdout: &str, stderr: &str) -> String {
    let raw = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if raw.is_empty() {
        "command failed without output".to_string()
    } else {
        raw.to_string()
    }
}
