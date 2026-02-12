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
//! 3. vx_fskit::teardown() to cleanly unmount the VFS

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
use figue as args;

/// vx-reaper - Process group reaper for macOS
#[cfg(target_os = "macos")]
#[derive(facet::Facet, Debug)]
struct Args {
    /// Parent PID to watch
    #[facet(args::named)]
    parent_pid: i32,

    /// VFS mount path to tear down
    #[facet(args::named)]
    mount_path: String,

    /// Log file path (redirects stderr)
    #[facet(args::named)]
    log_file: Option<String>,

    /// Standard CLI options
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Args = args::Driver::new(
        args::builder::<Args>()
            .unwrap()
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name("vx-reaper")
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

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

    let parent_pid = args.parent_pid as libc::pid_t;

    log!(
        "starting (parent-pid={}, mount-path={})",
        parent_pid,
        args.mount_path
    );

    // Check parent is still alive before we even start
    if unsafe { libc::kill(parent_pid, 0) } != 0 {
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

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async_main(parent_pid, args.mount_path));
}

#[cfg(target_os = "macos")]
async fn async_main(parent_pid: libc::pid_t, mount_path: String) {
    use tokio::io::AsyncBufReadExt;

    let mut pgroups = std::collections::HashSet::<libc::pid_t>::new();

    // Spawn a blocking task that watches the parent via kqueue EVFILT_PROC.
    let (parent_dead_tx, parent_dead_rx) = peeps_sync::oneshot_channel::<()>("parent_dead");

    tokio::task::spawn_blocking(move || {
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            log!("kqueue() failed: {}", std::io::Error::last_os_error());
            let _ = parent_dead_tx.send(());
            return;
        }

        let proc_event = libc::kevent {
            ident: parent_pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
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
                "EVFILT_PROC registration failed ({}), falling back to polling",
                std::io::Error::last_os_error()
            );
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if unsafe { libc::kill(parent_pid, 0) } != 0 {
                    break;
                }
            }
        } else {
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            loop {
                let n = unsafe {
                    libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null())
                };
                if n > 0 {
                    break;
                }
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    log!("kevent wait failed: {err}");
                    break;
                }
            }
        }
        unsafe { libc::close(kq) };
        let _ = parent_dead_tx.send(());
    });

    // Read PGIDs from stdin until parent dies or stdin closes.
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    tokio::select! {
        _ = async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(pgid) = line.trim().parse::<libc::pid_t>() {
                    log!("registered pgroup {pgid}");
                    pgroups.insert(pgid);
                }
            }
            log!("stdin EOF, starting cleanup");
        } => {}
        _ = parent_dead_rx.recv() => {
            log!("parent exited (kqueue), starting cleanup");
        }
    }

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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Phase 3: teardown the VFS mount
    let mount_path = camino::Utf8PathBuf::from(&mount_path);
    log!("tearing down {mount_path}");
    let result = vx_fskit::teardown(&mount_path).await;
    for entry in &result.log {
        log!("{}", entry.message);
    }
    if let Some(error) = &result.error {
        log!("teardown failed: {}", error.summary);
        for remediation in &error.remediation {
            log!("  try: {remediation}");
        }
    }
    log!("done");
}
