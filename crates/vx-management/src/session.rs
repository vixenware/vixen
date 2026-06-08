//! Active console-session helpers.
//!
//! Most privileged work stays in the root management service. FSKit enablement,
//! however, is user-session state, so the final `/sbin/mount` invocation must run in
//! the active console user's launchd bootstrap while keeping the caller's root
//! privileges.

use std::ffi::{CStr, CString};

use eyre::{Result, WrapErr};

#[derive(Debug, Clone)]
pub struct ConsoleUser {
    pub uid: u32,
    pub name: String,
}

pub fn console_user() -> Result<Option<ConsoleUser>> {
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
