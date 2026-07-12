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
            (libc::SYS_read, vec![]),
            (libc::SYS_pread64, vec![]),
            (libc::SYS_preadv, vec![]),
            (libc::SYS_readv, vec![]),
            (libc::SYS_write, vec![]),
            (libc::SYS_getcwd, vec![]),
            (libc::SYS_mprotect, vec![]),
            (libc::SYS_open, vec![]),
            (libc::SYS_openat, vec![]),
            (libc::SYS_close, vec![]),
            (libc::SYS_stat, vec![]),
            (libc::SYS_fstat, vec![]),
            (libc::SYS_statx, vec![]),
            (libc::SYS_lstat, vec![]),
            (libc::SYS_newfstatat, vec![]),
            (libc::SYS_access, vec![]),
            (libc::SYS_mmap, vec![]),
            (libc::SYS_munmap, vec![]),
            (libc::SYS_lseek, vec![]),
            (libc::SYS_sigaltstack, vec![]),
            (libc::SYS_exit_group, vec![]),
            (libc::SYS_exit, vec![]),
            (libc::SYS_readlink, vec![]),
            (libc::SYS_readlinkat, vec![]),
            (libc::SYS_gettid, vec![]),
            (libc::SYS_futex, vec![]),
            (libc::SYS_get_robust_list, vec![]),
            (libc::SYS_set_robust_list, vec![]),
            (libc::SYS_rt_sigaction, vec![]),
            (libc::SYS_brk, vec![]),
            (libc::SYS_arch_prctl, vec![]),
            (libc::SYS_prctl, vec![]),
            (libc::SYS_set_tid_address, vec![]),
            (libc::SYS_membarrier, vec![]),
            (libc::SYS_rseq, vec![]),
            (libc::SYS_prlimit64, vec![]),
            (libc::SYS_getrandom, vec![]),
            (libc::SYS_poll, vec![]),
            (libc::SYS_ppoll, vec![]),
            (libc::SYS_ioctl, vec![]),
            (libc::SYS_rt_sigprocmask, vec![]),
            (libc::SYS_sched_getaffinity, vec![]),
            (libc::SYS_epoll_create, vec![]),
            (libc::SYS_epoll_create1, vec![]),
            (libc::SYS_epoll_ctl, vec![]),
            (libc::SYS_epoll_wait, vec![]),
            (libc::SYS_eventfd, vec![]),
            (libc::SYS_eventfd2, vec![]),
            (libc::SYS_madvise, vec![]),
            (libc::SYS_uname, vec![]),
            (libc::SYS_getpid, vec![]),
            (libc::SYS_pipe, vec![]),
            (libc::SYS_pipe2, vec![]),
            (libc::SYS_getdents64, vec![]),
            (libc::SYS_getdents, vec![]),
            (libc::SYS_fcntl, vec![]),
            (libc::SYS_execve, vec![]),
            (libc::SYS_clone, vec![]),
            (libc::SYS_clone3, vec![]),
            (libc::SYS_vfork, vec![]),
        ]
        .into(),
        SeccompAction::Errno(libc::EPERM as u32),
        SeccompAction::Allow,
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
