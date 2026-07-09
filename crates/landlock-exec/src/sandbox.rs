use crate::{LandlockProfile, TARGET_LANDLOCK_ABI};

use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

#[cfg(target_endian = "big")]
compile_error!("seccomp does not support big-endian architectures");

pub fn exec_under_sandbox(
    profile: &LandlockProfile,
    program: impl AsRef<Path>,
    args: &[String],
) -> Result<(), String> {
    apply_to_current_thread(profile)?;
    apply_network_seccomp()?;

use std::fs::{self, canonicalize};
use std::os::unix::process::CommandExt;

use std::path::PathBuf;

use std::process::Command;

#[derive(Debug)]
struct Args {
    vfs_root: PathBuf,
    write_dir: PathBuf,
    program: String,
    program_args: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut vfs_root = None;
    let mut write_dir = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--vfs-root" => {
                vfs_root = Some(args.next().ok_or("--vfs-root requires a path argument")?);
            }
            "--write-dir" => {
                write_dir = Some(args.next().ok_or("--write-dir requires a path argument")?);
            }
            "--" => break,
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let program = args.next().ok_or("no program specified after --")?;
    let program_args: Vec<String> = args.collect();

    Ok(Args {
        vfs_root: vfs_root.ok_or("missing required --vfs-root")?.into(),
        write_dir: write_dir.ok_or("missing required --write-dir")?.into(),
        program,
        program_args,
    })
}

pub fn run() -> Result<(), String> {
    let args = parse_args()?;

    eprintln!("landlock-exec: setting up sandbox");
    eprintln!("  vfs_root: {}", args.vfs_root.display());
    eprintln!("  write_dir: {}", args.write_dir.display());
    eprintln!("  program: {}", args.program);

    // Try ABI versions from newest to oldest until one works
    let abis = [
        (ABI::V4, "V4 (5.19+)"),
        (ABI::V3, "V3 (5.19+)"),
        (ABI::V2, "V2 (5.19+)"),
        (ABI::V1, "V1 (5.13+)"),
    ];

    let mut last_error = None;
    let mut ruleset = None;

    for (abi, desc) in abis {
        eprintln!("landlock-exec: trying ABI {desc}");

        let fs_read_only = AccessFs::from_read(abi);
        let fs_read_write = AccessFs::from_all(abi);

        match Ruleset::default().handle_access(fs_read_write) {
            Ok(rs) => {
                eprintln!("landlock-exec: ABI {desc} supported");
                ruleset = Some((rs, fs_read_only, fs_read_write));
                break;
            }
            Err(e) => {
                eprintln!("landlock-exec: ABI {desc} not supported: {e}");
                last_error = Some(e);
            }
        }
    }

    let (ruleset, fs_read_only, fs_read_write) = ruleset.ok_or_else(|| {
        format!(
            "landlock not supported by kernel (requires 5.13+): {:?}",
            last_error
        )
    })?;

    eprintln!("landlock-exec: creating ruleset");

    // Create the ruleset
    let mut created = ruleset
        .create()
        .map_err(|e| format!("failed to create ruleset: {e}"))?;

    // Add read-only access to VFS root
    created = created
        .add_rule(PathBeneath::new(
            PathFd::new(&args.vfs_root)
                .map_err(|e| format!("failed to open vfs_root {}: {e}", args.vfs_root.display()))?,
            fs_read_only,
        ))
        .map_err(|e| format!("failed to add vfs_root rule: {e}"))?;

    // Add read-write access to output directory
    created = created
        .add_rule(PathBeneath::new(
            PathFd::new(&args.write_dir).map_err(|e| {
                format!("failed to open write_dir {}: {e}", args.write_dir.display())
            })?,
            fs_read_write,
        ))
        .map_err(|e| format!("failed to add write_dir rule: {e}"))?;

    // Add read-write access to /tmp for rustc temp files (rustc ignores TMPDIR)
    if let Ok(fd) = PathFd::new("/tmp") {
        eprintln!("landlock-exec: adding read-write access to /tmp");
        created = created
            .add_rule(PathBeneath::new(fd, fs_read_write))
            .map_err(|e| format!("failed to add /tmp rule: {e}"))?;
    } else {
        eprintln!("landlock-exec: warning: /tmp does not exist");
    }

    // Add read-only access to system paths for toolchain (only if they exist)
    let system_paths = ["/usr", "/lib", "/lib64", "/etc", "/home"];
    for path in system_paths {
        if let Ok(fd) = PathFd::new(path) {
            eprintln!("landlock-exec: adding read access to {path}");
            created = created
                .add_rule(PathBeneath::new(fd, fs_read_only))
                .map_err(|e| format!("failed to add {path} rule: {e}"))?;
        } else {
            eprintln!("landlock-exec: skipping {path} (does not exist)");
        }
    }

    // Apply the sandbox to this process
    let status = created
        .restrict_self()
        .map_err(|e| format!("failed to restrict_self: {e}"))?;

    eprintln!("landlock-exec: sandbox applied successfully");
    eprintln!("  ruleset status: {:?}", status.ruleset);
    eprintln!("  no_new_privs: {}", status.no_new_privs);

    // Verify sandbox is fully enforced
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(format!("sandbox not fully enforced: {:?}", status.ruleset));
    }

    // Defense-in-depth: re-set PR_SET_PDEATHSIG right before exec.
    // vx-runner's pre_exec already sets this, and it survives exec(), but
    // re-setting it here guards against anything that might have cleared it.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
    }

    let err = Command::new(program.as_ref()).args(args).exec();
    Err(format!("exec failed: {err}"))
}

pub fn apply_to_current_thread(profile: &LandlockProfile) -> Result<(), String> {
    eprintln!("landlock-exec: setting up sandbox");
    eprintln!("  target ABI: {}", TARGET_LANDLOCK_ABI);
    log_roots("read_root", profile.read_roots());
    log_roots("write_root", profile.write_roots());
    log_roots("exec_root", profile.exec_roots());
    let filter: BpfProgram = SeccompFilter::new(
        [
            (libc::SYS_read, vec![]),            // [I/O] Read from file descr.
            (libc::SYS_pread64, vec![]),         // [I/O] Read pointer of file
            (libc::SYS_preadv, vec![]),          // [I/O] Read buffer of pointers of file
            (libc::SYS_readv, vec![]),           // [I/O] Read buffer from file
            (libc::SYS_write, vec![]),           // [I/O] Writing to file descr.
            (libc::SYS_getcwd, vec![]),          // [I/O] Getting the working dir
            (libc::SYS_mprotect, vec![]),        // Protecting a region of memory
            (libc::SYS_open, vec![]),            // [I/O] Open file
            (libc::SYS_openat, vec![]),          // [I/O] Open file
            (libc::SYS_close, vec![]),           // [I/O] Close files
            (libc::SYS_stat, vec![]),            // [I/O] Getting file metadata
            (libc::SYS_fstat, vec![]),           // [I/O] Identical to stat
            (libc::SYS_statx, vec![]),           // [I/O] Getting file metadata (extended)
            (libc::SYS_lstat, vec![]),           // [I/O] Getting symlink's metadata
            (libc::SYS_newfstatat, vec![]),      // Inner function by fstatat on glibc
            (libc::SYS_access, vec![]), // Check user's perms for a file. TODO: Look into this further
            (libc::SYS_mmap, vec![]),   // Create new mapping in virtual address space for process
            (libc::SYS_munmap, vec![]), // Create new mapping in virtual address space for process
            (libc::SYS_lseek, vec![]),  // Reposition the file offset
            (libc::SYS_sigaltstack, vec![]), // Reposition the file offset
            (libc::SYS_exit_group, vec![]), // Exit all threads in process
            (libc::SYS_exit, vec![]),   // Exit process
            (libc::SYS_readlink, vec![]), // Read value of symlink
            (libc::SYS_readlinkat, vec![]), // Read value of symlink
            (libc::SYS_gettid, vec![]), // Read value of symlink
            (libc::SYS_futex, vec![]),  // Fast user-space locking
            (libc::SYS_get_robust_list, vec![]), // Get list of robust futexes
            (libc::SYS_set_robust_list, vec![]), // Set list of robust futexes
            (libc::SYS_rt_sigaction, vec![]), // Examine and change signal action,
            (libc::SYS_brk, vec![]),    // Change data segment size
            (libc::SYS_arch_prctl, vec![]), // Set architecture-specific thread state
            (libc::SYS_prctl, vec![]), // Operations on a process or thread, See https://www.man7.org/linux/man-pages/man2/prctl.2.html
            (libc::SYS_set_tid_address, vec![]), // Set pointer to thread ID
            (libc::SYS_gettid, vec![]),
            (libc::SYS_membarrier, vec![]), // Issue memory barriers on a set of threads
            (libc::SYS_rseq, vec![]), // "Restartable sequences", per-CPU data structures without locking
            (libc::SYS_prlimit64, vec![]), // Get / Set resource limits for applications
            (libc::SYS_getrandom, vec![]), // Get random bytes
            (libc::SYS_poll, vec![]), // Wait for some event on a file descriptor
            (libc::SYS_ppoll, vec![]), // Wait for some event on a file descriptor
            (libc::SYS_ioctl, vec![]), // Manipulates the underlying device params of special files (like terminals).
            (libc::SYS_rt_sigprocmask, vec![]), // Examine and change blocked signals
            (libc::SYS_sched_getaffinity, vec![]), // Set and get a thread's CPU affinity mask
            (libc::SYS_epoll_create, vec![]), // Open an epoll file descriptor
            (libc::SYS_epoll_create1, vec![]), // Open an epoll file descriptor
            (libc::SYS_epoll_ctl, vec![]), // Control interface for an epoll file descriptor
            (libc::SYS_epoll_wait, vec![]),
            (libc::SYS_eventfd, vec![]), // Create a file descriptor for event notification
            (libc::SYS_eventfd2, vec![]), // Create a file descriptor for event notification
            (libc::SYS_madvise, vec![]), // Give advice about use of memory
            (libc::SYS_uname, vec![]),   // Get name and info about current kernel
            (libc::SYS_getpid, vec![]),  // Get Current process identification
            (libc::SYS_pipe, vec![]),    // Create a pipe (like in a shell)
            (libc::SYS_pipe2, vec![]),   // Create a pipe (like in a shell)
            (libc::SYS_getdents64, vec![]), // Get directory entries
            (libc::SYS_getdents, vec![]), // Get directory entries
            (libc::SYS_fcntl, vec![]),
            // ======================================= EXECUTION SYSCALL
            // We already do VFS and other filesystem-hardening, but be careful with these.
            (libc::SYS_execve, vec![]), // Execute!
            (libc::SYS_clone, vec![]),  // Create a child process
            (libc::SYS_clone3, vec![]), // Create a child process
                                        // ======================================= EXECUTION SYSCALL
        ]
        .into(),
        SeccompAction::Trap,  // For calls that mismatch
        SeccompAction::Allow, // For calls that match
        std::env::consts::ARCH.try_into().unwrap(),
    )
    .expect("Hard-coded Seccomp filter should not have any error!")
    .try_into()
    .expect("Hard-coded Seccomp filter should not have any error when translating!");

    let abi = ABI::V4;
    let read_access = AccessFs::ReadFile | AccessFs::ReadDir;
    let write_access = read_access | AccessFs::from_write(abi);
    let exec_access = read_access | AccessFs::Execute;
    let all_file_access = AccessFs::from_all(abi);
    let all_tcp_access = AccessNet::from_all(abi);

    let mut created = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all_file_access)
        .map_err(|err| {
            format!(
                "kernel does not satisfy required Landlock {TARGET_LANDLOCK_ABI} file rights: {err}"
            )
        })?
        .handle_access(all_tcp_access)
        .map_err(|err| {
            format!(
                "kernel does not satisfy required Landlock {TARGET_LANDLOCK_ABI} TCP rights: {err}"
            )
        })?
        .create()
        .map_err(|err| format!("failed to create Landlock ruleset: {err}"))?;

    for root in profile.read_roots() {
        created = add_path_rule(created, root, read_access, "read_root")?;
    }
    for root in profile.write_roots() {
        created = add_path_rule(created, root, write_access, "write_root")?;
    }
    for root in profile.exec_roots() {
        created = add_path_rule(created, root, exec_access, "exec_root")?;
    }

    let status = created
        .restrict_self()
        .map_err(|err| format!("failed to restrict_self: {err}"))?;

    eprintln!("landlock-exec: sandbox applied");
    eprintln!("  ruleset status: {:?}", status.ruleset);
    eprintln!("  no_new_privs: {}", status.no_new_privs);
    eprintln!("  landlock: {:?}", status.landlock);

    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(format!(
            "Landlock {TARGET_LANDLOCK_ABI} was not fully enforced: {:?}",
            status
        ));
    }

    if !status.no_new_privs {
        return Err("Landlock did not set no_new_privs".to_string());
    }
    eprintln!("landlock-exec: exec'ing {}", args.program);

    // Find the path to the executable we're going to execute, make sure we have PIE, ASLR and everything else
    let program = match which::which(args.program) {
        Ok(program) => program,
        Err(err) => return Err(format!("Could not get origin of program: {err}")),
    };

    // Replace this process with the target program
    let err = Command::new(&program).args(&args.program_args).exec(); // This never returns on success

    Ok(())
}

fn add_path_rule(
    created: landlock::RulesetCreated,
    root: &Path,
    access: landlock::BitFlags<AccessFs>,
    kind: &str,
) -> Result<landlock::RulesetCreated, String> {
    let access = access_for_path(root, access)
        .map_err(|err| format!("failed to inspect {kind} {}: {err}", root.display()))?;
    let fd = PathFd::new(root)
        .map_err(|err| format!("failed to open {kind} {}: {err}", root.display()))?;
    created
        .add_rule(PathBeneath::new(fd, access))
        .map_err(|err| format!("failed to add {kind} {}: {err}", root.display()))
}

fn access_for_path(
    root: &Path,
    access: landlock::BitFlags<AccessFs>,
) -> Result<landlock::BitFlags<AccessFs>, std::io::Error> {
    if root.metadata()?.is_dir() {
        Ok(access)
    } else {
        Ok(access & AccessFs::from_file(ABI::V4))
    }
}

fn apply_network_seccomp() -> Result<(), String> {
    let filter: BpfProgram = SeccompFilter::new(
        [
            (libc::SYS_accept, vec![]),
            (libc::SYS_accept4, vec![]),
            (libc::SYS_listen, vec![]),
        ]
        .into(),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        std::env::consts::ARCH
            .try_into()
            .map_err(|err| format!("unsupported seccomp architecture: {err}"))?,
    )
    .map_err(|err| format!("failed to build seccomp filter: {err}"))?
    .try_into()
    .map_err(|err| format!("failed to compile seccomp filter: {err}"))?;

    seccompiler::apply_filter(&filter)
        .map_err(|err| format!("failed to apply network seccomp filter: {err}"))
}

fn log_roots(label: &str, roots: &[std::path::PathBuf]) {
    for root in roots {
        eprintln!("  {label}: {}", root.display());
    }
}
