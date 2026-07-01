//! Landlock sandbox wrapper for compiler subprocesses (Linux).
//!
//! This binary applies landlock restrictions and then execs the target program.
//!
//! Usage:
//!
//!     landlock-exec --vfs-root <PATH> --write-dir <PATH> -- <program> [args...]
//!
//! Security model:
//! - File reads: restricted to VFS_ROOT + system paths (/usr, /lib, /etc for toolchain)
//! - File writes: restricted to WRITE_DIR + /tmp (rustc temp files, ignores TMPDIR)
//! - Network: denied entirely (no BindTcp/ConnectTcp rules added)
//! - Execution: allowed from VFS_ROOT + system paths

// Set global allocator to GrapheneOS allocator.
#[cfg(target_pointer_width = "64")]
#[global_allocator]
static GLOBAL: hardened_malloc::HardenedMalloc = hardened_malloc::HardenedMalloc;

#[cfg(target_os = "linux")]
use magic::cookie;
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};

#[cfg(target_endian = "big")]
compile_error!("seccomp does not support big-endian architectures");

fn main() {
    // Harden process before ANYTHING else
    if let Err(e) = secmem_proc::harden_process() {
        println!("ERROR: could not harden process, exiting");
        println!("ERROR: {}", e);
        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("landlock-exec: this binary only works on Linux");
        std::process::exit(1);
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = run() {
            eprintln!("landlock-exec error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
use landlock::{
    ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus,
};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct Args {
    vfs_root: PathBuf,
    write_dir: PathBuf,
    program: String,
    program_args: Vec<String>,
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn run() -> Result<(), String> {
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
    let system_paths = ["/usr", "/lib", "/lib64", "/etc"];
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

    eprintln!("landlock-exec: creating filter");

    let filter: BpfProgram = SeccompFilter::new(
        [
            (libc::SYS_accept4, vec![]), // Explicit cancel on socket connection
            (
                libc::SYS_fcntl,
                vec![
                    SeccompRule::new(vec![
                        SeccompCondition::new(
                            1,
                            SeccompCmpArgLen::Dword,
                            SeccompCmpOp::Eq,
                            libc::F_SETFD as u64, // Allow set file descr. flags
                        )
                        .unwrap(),
                        SeccompCondition::new(
                            2,
                            SeccompCmpArgLen::Dword,
                            SeccompCmpOp::Eq,
                            libc::FD_CLOEXEC as u64, // Allow for setting if the file should code on succesful exec.
                        )
                        .unwrap(),
                    ])
                    .unwrap(),
                    SeccompRule::new(vec![
                        SeccompCondition::new(
                            1,
                            SeccompCmpArgLen::Dword,
                            SeccompCmpOp::Eq,
                            libc::F_GETFD as u64, // Allow for getting file descr. flags
                        )
                        .unwrap(),
                    ])
                    .unwrap(),
                ],
            ),
        ]
        .into(),
        SeccompAction::Trap,  // For calls that mismatch
        SeccompAction::Allow, // For calls that match
        std::env::consts::ARCH.try_into().unwrap(),
    )
    .expect("Hard-coded Seccomp filter should not have any error!")
    .try_into()
    .expect("Hard-coded Seccomp filter should not have any error when translating!");

    seccompiler::apply_filter(&filter).unwrap();

    eprintln!("landlock-exec: exec'ing {}", args.program);

    // Find the path to the executable we're going to execute, make sure we have PIE, ASLR and everything else
    let (file, program) = match which::which(&args.program) {
        Ok(program) => {
            let cookie = magic::Cookie::open(cookie::Flags::default())
                .expect("Could not open Cookie with default flags");

            let cookie = cookie
                .load(&cookie::DatabasePaths::default())
                .expect("Could not open DatabasePaths");

            (
                cookie.file(&program).expect(&format!(
                    "Could not analyze file `{:?}` with magic",
                    program
                )),
                program,
            )
        }
        Err(err) => return Err(format!("Could not get origin of program: {err}")),
    };

    if !file.contains("ELF") {
        return Err(format!(
            "`{}` is not a valid ELF file, got: {file}",
            &program.to_string_lossy()
        ));
    } else if !file.contains("PIE") {
        return Err(format!(
            "`{}` is not a PIE ELF (Position Independent Executable ELF)",
            program.to_string_lossy()
        ));
    }

    // Replace this process with the target program
    let err = Command::new(&program).args(&args.program_args).exec(); // This never returns on success

    Err(format!("exec failed: {err}"))
}
