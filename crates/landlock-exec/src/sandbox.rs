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
