//! Process group reaper for macOS.
//!
//! Usage: vx-reaper --parent-pid <PID>
//!
//! Reads process group IDs from stdin (one per line) as vx-runner creates
//! them. Watches the parent PID using kqueue EVFILT_PROC. When the parent
//! exits, kills ALL known process groups with SIGKILL, then exits.
//!
//! vx-runner spawns this once at startup. Each time it spawns a sandboxed
//! command (which gets its own pgroup via setpgid), it writes the pgid to
//! the reaper's stdin. If vx-runner dies (even from SIGKILL), this process
//! detects it and kills every child pgroup, preventing dsymutil/ld/etc.
//! from getting stuck in uninterruptible sleep on a dead FSKit mount.
//!
//! Protocol: vx-runner writes "<pgid>\n" lines to the reaper's stdin.
//! The reaper accumulates them and kills them all when the parent dies.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("vx-reaper: this binary is only needed on macOS");
    eprintln!("vx-reaper: on Linux, use PR_SET_PDEATHSIG instead");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let parent_pid = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vx-reaper: {e}");
            eprintln!("usage: vx-reaper --parent-pid <PID>");
            std::process::exit(1);
        }
    };

    // Check parent is still alive before we even start
    if unsafe { libc::kill(parent_pid, 0) } != 0 {
        std::process::exit(0);
    }

    // Set up kqueue to watch parent exit AND stdin readability
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        eprintln!(
            "vx-reaper: kqueue() failed: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }

    let changes = [
        // Watch parent PID for exit
        libc::kevent {
            ident: parent_pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::without_provenance_mut(1), // tag: 1 = parent exit
        },
        // Watch stdin for readability (new pgid lines)
        libc::kevent {
            ident: libc::STDIN_FILENO as usize,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD,
            fflags: 0,
            data: 0,
            udata: std::ptr::without_provenance_mut(2), // tag: 2 = stdin data
        },
    ];

    let ret = unsafe {
        libc::kevent(
            kq,
            changes.as_ptr(),
            changes.len() as i32,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if ret < 0 {
        eprintln!(
            "vx-reaper: kevent register failed: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }

    let mut pgroups: Vec<libc::pid_t> = Vec::new();
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
            eprintln!("vx-reaper: kevent wait failed: {err}");
            std::process::exit(1);
        }

        if n == 0 {
            continue;
        }

        let tag = event.udata as usize;

        if tag == 1 {
            // Parent exited — kill everything
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
                // EOF or error on stdin — parent closed its end, which also means parent is dying
                break;
            }

            stdin_buf.push_str(&String::from_utf8_lossy(&read_buf[..bytes_read as usize]));

            // Process complete lines
            while let Some(newline_pos) = stdin_buf.find('\n') {
                let line = &stdin_buf[..newline_pos];
                if let Ok(pgid) = line.trim().parse::<libc::pid_t>() {
                    pgroups.push(pgid);
                }
                stdin_buf.drain(..=newline_pos);
            }
        }
    }

    // Kill all known process groups
    for &pgid in &pgroups {
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }

    unsafe {
        libc::close(kq);
    }
}

#[cfg(target_os = "macos")]
fn parse_args() -> Result<libc::pid_t, String> {
    let mut args = std::env::args().skip(1);
    let mut parent_pid: Option<libc::pid_t> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--parent-pid" => {
                let val = args.next().ok_or("--parent-pid requires a value")?;
                parent_pid = Some(
                    val.parse()
                        .map_err(|_| format!("invalid --parent-pid: {val}"))?,
                );
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    parent_pid.ok_or("missing required --parent-pid".to_string())
}
