//! Watch for cargo-nextest process death in test mode.
//!
//! When vxd runs under `VX_TEST=1`, this module finds the cargo-nextest
//! ancestor process and watches for it to exit. Once nextest is gone,
//! there's no point keeping the daemon alive — shut down immediately
//! instead of waiting for the idle timeout.

#![allow(unsafe_code)]

/// Walk up the process tree looking for a `cargo-nextest` ancestor.
///
/// Returns the PID if found, or `None` if we hit PID 0/1 or 32 hops
/// without finding it.
pub fn find_nextest_ancestor() -> Option<u32> {
    let mut pid = std::process::id();

    for _ in 0..32 {
        let ppid = get_parent_pid(pid)?;
        if ppid == 0 || ppid == 1 {
            return None;
        }

        if is_cargo_nextest(ppid) {
            return Some(ppid);
        }

        pid = ppid;
    }

    None
}

/// Wait for a process to exit.
pub async fn wait_for_exit(pid: u32) {
    imp::wait_for_exit(pid).await;
}

// --- Platform: macOS ---

#[cfg(target_os = "macos")]
mod imp {
    /// Get the parent PID of a process.
    pub fn get_parent_pid(pid: u32) -> Option<u32> {
        // Use proc_pidinfo with PROC_PIDTBSDINFO to get parent PID
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

        let ret = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };

        if ret <= 0 {
            return None;
        }

        Some(info.pbi_ppid)
    }

    /// Check if a process is cargo-nextest by examining its executable path.
    pub fn is_cargo_nextest(pid: u32) -> bool {
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let ret = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len() as u32,
            )
        };

        if ret <= 0 {
            return false;
        }

        let path = &buf[..ret as usize];
        let path = std::str::from_utf8(path).unwrap_or("");

        // Check if the basename is "cargo-nextest"
        path.rsplit('/').next() == Some("cargo-nextest")
    }

    /// Wait for a process to exit using kqueue EVFILT_PROC + NOTE_EXIT.
    pub async fn wait_for_exit(pid: u32) {
        tokio::task::spawn_blocking(move || {
            wait_for_exit_blocking(pid);
        })
        .await
        .ok();
    }

    fn wait_for_exit_blocking(pid: u32) {
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            tracing::warn!("kqueue() failed, falling back to polling");
            poll_until_dead(pid);
            return;
        }

        let changelist = [libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        }];

        let mut eventlist = [unsafe { std::mem::zeroed::<libc::kevent>() }];

        // Register the event. If the process is already dead, kevent returns
        // immediately, so we don't need to pre-check.
        let ret = unsafe {
            libc::kevent(
                kq,
                changelist.as_ptr(),
                changelist.len() as libc::c_int,
                eventlist.as_mut_ptr(),
                eventlist.len() as libc::c_int,
                std::ptr::null(), // no timeout — block forever
            )
        };

        unsafe { libc::close(kq) };

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(%err, "kevent() failed, falling back to polling");
            poll_until_dead(pid);
        }
    }

    fn poll_until_dead(pid: u32) {
        loop {
            let ret = unsafe { libc::kill(pid as libc::c_int, 0) };
            if ret != 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

// --- Platform: Linux ---

#[cfg(target_os = "linux")]
mod imp {
    use std::os::fd::FromRawFd;

    /// Get the parent PID of a process by reading /proc/{pid}/stat.
    pub fn get_parent_pid(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Format: "pid (comm) state ppid ..."
        // comm can contain spaces and parens, so find the last ')' first
        let after_comm = stat.rfind(')')? + 2; // skip ") "
        let fields: Vec<&str> = stat[after_comm..].split_whitespace().collect();
        // fields[0] = state, fields[1] = ppid
        fields.get(1)?.parse().ok()
    }

    /// Check if a process is cargo-nextest by reading /proc/{pid}/exe.
    pub fn is_cargo_nextest(pid: u32) -> bool {
        let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
            return false;
        };
        exe.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "cargo-nextest")
            .unwrap_or(false)
    }

    /// Wait for a process to exit using pidfd_open + poll.
    pub async fn wait_for_exit(pid: u32) {
        // Try pidfd_open first (Linux 5.3+)
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_int, 0) };

        if pidfd >= 0 {
            let fd = pidfd as std::os::fd::RawFd;
            // Use AsyncFd for non-blocking wait
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            if let Ok(async_fd) = tokio::io::unix::AsyncFd::new(owned) {
                // Wait for the fd to become readable (process exited)
                let _ = async_fd.readable().await;
                return;
            }
        }

        // Fallback: poll kill(pid, 0)
        loop {
            let ret = unsafe { libc::kill(pid as libc::c_int, 0) };
            if ret != 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

// --- Unsupported platforms ---

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn get_parent_pid(_pid: u32) -> Option<u32> {
        None
    }

    pub fn is_cargo_nextest(_pid: u32) -> bool {
        false
    }

    pub async fn wait_for_exit(_pid: u32) {
        // Can't watch processes on this platform, just poll
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    }
}

fn get_parent_pid(pid: u32) -> Option<u32> {
    imp::get_parent_pid(pid)
}

fn is_cargo_nextest(pid: u32) -> bool {
    imp::is_cargo_nextest(pid)
}
