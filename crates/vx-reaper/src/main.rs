//! Process group reaper for macOS.
//!
//! Usage: vx-reaper --parent-pid <PID> --mount-path <PATH> [--log-file <PATH>]
//!
//! Reads process group IDs from stdin (one per line) as vx-runner creates
//! them. Watches the parent PID using kqueue EVFILT_PROC (with stdin EOF
//! as fallback if EVFILT_PROC registration fails). When the parent exits:
//!
//! 1. SIGKILL all known child process groups
//! 2. Brief sleep to let SIGKILL propagate
//! 3. Force-unmount the VFS mount point (with timeout + last-resort pkill)
//!
//! Protocol: vx-runner writes "<pgid>\n" lines to the reaper's stdin.
//! The reaper accumulates them in a HashSet and reaps them all when the
//! parent dies.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("vx-reaper: this binary is only needed on macOS");
    eprintln!("vx-reaper: on Linux, use PR_SET_PDEATHSIG instead");
    std::process::exit(1);
}

/// Log with PID and ISO-8601 timestamp prefix.
#[cfg(target_os = "macos")]
macro_rules! log {
    ($($arg:tt)*) => {{
        eprintln!(
            "{} {} {}",
            std::process::id(),
            jiff::Zoned::now(),
            format_args!($($arg)*)
        );
    }};
}

#[cfg(target_os = "macos")]
fn main() {
    let args = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vx-reaper: {e}");
            eprintln!(
                "usage: vx-reaper --parent-pid <PID> --mount-path <PATH> [--log-file <PATH>]"
            );
            std::process::exit(1);
        }
    };

    // Redirect stderr to log file if specified
    if let Some(ref log_file) = args.log_file {
        let fd = unsafe {
            libc::open(
                std::ffi::CString::new(log_file.as_str()).unwrap().as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                0o644,
            )
        };
        if fd >= 0 {
            unsafe {
                libc::dup2(fd, libc::STDERR_FILENO);
                libc::close(fd);
            }
        }
    }

    log!(
        "starting (parent-pid={}, mount-path={})",
        args.parent_pid,
        args.mount_path
    );

    // Check parent is still alive before we even start
    if unsafe { libc::kill(args.parent_pid, 0) } != 0 {
        std::process::exit(0);
    }

    // Set stdin to close-on-exec so grandchildren can't accidentally
    // inherit it and keep us alive
    unsafe {
        let flags = libc::fcntl(libc::STDIN_FILENO, libc::F_GETFD);
        if flags != -1 {
            libc::fcntl(libc::STDIN_FILENO, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }

    // Set up kqueue to watch parent exit AND stdin readability
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        log!("kqueue() failed: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }

    // Register stdin readability first (always works)
    let stdin_event = libc::kevent {
        ident: libc::STDIN_FILENO as usize,
        filter: libc::EVFILT_READ,
        flags: libc::EV_ADD,
        fflags: 0,
        data: 0,
        udata: std::ptr::without_provenance_mut(2), // tag: 2 = stdin data
    };
    let ret = unsafe {
        libc::kevent(
            kq,
            &stdin_event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if ret < 0 {
        log!(
            "kevent register stdin failed: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }

    // Attempt EVFILT_PROC on parent — may fail (EPERM/ESRCH), that's OK,
    // stdin EOF is our fallback
    let proc_event = libc::kevent {
        ident: args.parent_pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::without_provenance_mut(1), // tag: 1 = parent exit
    };
    let ret = unsafe {
        libc::kevent(
            kq,
            &proc_event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if ret < 0 {
        log!(
            "EVFILT_PROC registration failed ({}), relying on stdin EOF",
            std::io::Error::last_os_error()
        );
    }

    let mut pgroups = std::collections::HashSet::<libc::pid_t>::new();
    let mut stdin_buf = String::new();
    let mut read_buf = [0u8; 256];

    loop {
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let n = unsafe { libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) };

        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            log!("kevent wait failed: {err}");
            std::process::exit(1);
        }

        if n == 0 {
            continue;
        }

        let tag = event.udata as usize;

        if tag == 1 {
            // Parent exited — time to reap
            log!("parent exited (EVFILT_PROC), starting cleanup");
            break;
        }

        if tag == 2 {
            // Stdin has data — read and parse pgid lines
            let bytes_read = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    read_buf.as_mut_ptr().cast(),
                    read_buf.len(),
                )
            };

            if bytes_read <= 0 {
                // EOF on stdin — parent closed its end
                log!("stdin EOF, starting cleanup");
                break;
            }

            stdin_buf.push_str(&String::from_utf8_lossy(&read_buf[..bytes_read as usize]));

            // Process complete lines
            while let Some(newline_pos) = stdin_buf.find('\n') {
                let line = &stdin_buf[..newline_pos];
                if let Ok(pgid) = line.trim().parse::<libc::pid_t>() {
                    log!("registered pgroup {pgid}");
                    pgroups.insert(pgid);
                }
                stdin_buf.drain(..=newline_pos);
            }
        }
    }

    unsafe { libc::close(kq) };

    // Phase 1: SIGKILL all known process groups (double tap)
    log!("killing {} pgroup(s)", pgroups.len());
    for &pgid in &pgroups {
        log!("SIGKILL pgroup {pgid}");
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
            libc::kill(pgid, libc::SIGKILL);
        }
    }

    // Phase 2: brief sleep to let SIGKILL propagate
    // (We can't reliably verify process group death without scanning all
    // processes — kill(pgid, 0) checks a single PID, not the group.)
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Phase 3: force-unmount with timeout
    log!("unmounting {}", args.mount_path);
    force_unmount(&args.mount_path);
    log!("done");
}

/// Force-unmount a path with timeout and last-resort pkill.
#[cfg(target_os = "macos")]
fn force_unmount(mount_path: &str) {
    use std::process::Command;

    let mut child = match Command::new("diskutil")
        .arg("unmount")
        .arg("force")
        .arg(mount_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            log!("failed to spawn diskutil: {e}");
            return;
        }
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return,
            Ok(None) => {}
            Err(e) => {
                log!("failed to wait for diskutil: {e}");
                return;
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Timed out — kill diskutil and try killing the FSKit extension as last resort
    log!("diskutil unmount force timed out, killing diskutil");
    let _ = child.kill();
    let _ = child.wait();

    log!("killing VixenFS extension as last resort");
    let _ = Command::new("pkill")
        .arg("-x")
        .arg("VixenFS")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(target_os = "macos")]
struct Args {
    parent_pid: libc::pid_t,
    mount_path: String,
    log_file: Option<String>,
}

#[cfg(target_os = "macos")]
fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut parent_pid: Option<libc::pid_t> = None;
    let mut mount_path: Option<String> = None;
    let mut log_file: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--parent-pid" => {
                let val = args.next().ok_or("--parent-pid requires a value")?;
                parent_pid = Some(
                    val.parse()
                        .map_err(|_| format!("invalid --parent-pid: {val}"))?,
                );
            }
            "--mount-path" => {
                mount_path = Some(args.next().ok_or("--mount-path requires a value")?);
            }
            "--log-file" => {
                log_file = Some(args.next().ok_or("--log-file requires a value")?);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        parent_pid: parent_pid.ok_or("missing required --parent-pid")?,
        mount_path: mount_path.ok_or("missing required --mount-path")?,
        log_file,
    })
}
