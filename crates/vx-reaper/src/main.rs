//! Process group reaper for macOS.
//!
//! Usage: vx-reaper --parent-pid <PID> --mount-path <PATH>
//!
//! Reads process group IDs from stdin (one per line) as vx-runner creates
//! them. Watches the parent PID using kqueue EVFILT_PROC. When the parent
//! exits:
//!
//! 1. SIGKILL all known child process groups
//! 2. Verify they're dead (kill(pid, 0) polling)
//! 3. Unmount the VFS mount point (diskutil unmount)
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

#[cfg(target_os = "macos")]
fn main() {
    let args = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vx-reaper: {e}");
            eprintln!("usage: vx-reaper --parent-pid <PID> --mount-path <PATH>");
            std::process::exit(1);
        }
    };

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
        eprintln!(
            "vx-reaper: kqueue() failed: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }

    let changes = [
        // Watch parent PID for exit
        libc::kevent {
            ident: args.parent_pid as usize,
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
            eprintln!("vx-reaper: kevent wait failed: {err}");
            std::process::exit(1);
        }

        if n == 0 {
            continue;
        }

        let tag = event.udata as usize;

        if tag == 1 {
            // Parent exited — time to reap
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
                break;
            }

            stdin_buf.push_str(&String::from_utf8_lossy(&read_buf[..bytes_read as usize]));

            // Process complete lines
            while let Some(newline_pos) = stdin_buf.find('\n') {
                let line = &stdin_buf[..newline_pos];
                if let Ok(pgid) = line.trim().parse::<libc::pid_t>() {
                    pgroups.insert(pgid);
                }
                stdin_buf.drain(..=newline_pos);
            }
        }
    }

    unsafe { libc::close(kq) };

    // Phase 1: SIGKILL all known process groups (double tap)
    for &pgid in &pgroups {
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
            libc::kill(pgid, libc::SIGKILL);
        }
    }

    // Phase 2: verify they're dead
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut alive: Vec<libc::pid_t> = pgroups.iter().copied().collect();
    while !alive.is_empty() && std::time::Instant::now() < deadline {
        alive.retain(|&pgid| unsafe { libc::kill(pgid, 0) } == 0);
        if !alive.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // Phase 3: unmount (force — the VFS server process is already dead)
    let _ = std::process::Command::new("diskutil")
        .arg("unmount")
        .arg("force")
        .arg(&args.mount_path)
        .output();
}

#[cfg(target_os = "macos")]
struct Args {
    parent_pid: libc::pid_t,
    mount_path: String,
}

#[cfg(target_os = "macos")]
fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut parent_pid: Option<libc::pid_t> = None;
    let mut mount_path: Option<String> = None;

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
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        parent_pid: parent_pid.ok_or("missing required --parent-pid")?,
        mount_path: mount_path.ok_or("missing required --mount-path")?,
    })
}
