use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PlatformExecRequest<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub env: &'a [(String, String)],
    pub sandbox_root: &'a Path,
}

#[derive(Debug, Clone)]
pub struct PlatformExecOutput {
    pub exit_code: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn run_platform_exec(request: PlatformExecRequest<'_>) -> std::io::Result<PlatformExecOutput> {
    let mut command = sandboxed_command(request.program, request.sandbox_root);
    hermetic_env(&mut command, request.sandbox_root);
    command.current_dir(request.sandbox_root);
    for arg in request.args {
        command.arg(arg);
    }
    for (name, value) in request.env {
        command.env(name, value);
    }
    let output = command.output()?;
    let exit_code = output
        .status
        .code()
        .unwrap_or_else(|| if output.status.success() { 0 } else { 1 });
    Ok(PlatformExecOutput {
        exit_code: i64::from(exit_code),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn hermetic_env(command: &mut Command, write_root: &Path) {
    command.env_clear();
    for var in [
        "PATH",
        "HOME",
        "LIB",
        "INCLUDE",
        "LIBPATH",
        "WindowsSdkDir",
        "WindowsSDKVersion",
        "CARGO_HOME",
        "RUSTUP_HOME",
    ] {
        if let Ok(value) = std::env::var(var) {
            command.env(var, value);
        }
    }
    command.env("TMPDIR", write_root);
    command.env("TMP", write_root);
    command.env("TEMP", write_root);
}

#[cfg(target_os = "macos")]
const SEATBELT_PROFILE_BASE: &str = r#"(version 1)
(debug deny)
(deny default)
(import "system.sb")
(allow process-fork)
(allow process-exec (subpath (param "VFS_ROOT")))
(allow process-exec (subpath (param "VFS_ROOT_ALT")))
(allow process-exec (subpath "/bin"))
(allow process-exec (subpath "/usr/bin"))
(allow process-exec (subpath "/Library/Developer/CommandLineTools"))
(allow file-read* (subpath (param "VFS_ROOT")))
(allow file-read* (subpath (param "VFS_ROOT_ALT")))
(allow file-read* (subpath "/bin"))
(allow file-read* (subpath "/usr/bin"))
(allow file-read* (subpath "/Library/Developer/CommandLineTools"))
(allow file-read* (subpath "/usr"))
(allow file-read* (subpath "/System"))
(allow file-read* (subpath "/Library"))
(allow file-read* (subpath "/private"))
(allow file-write* (subpath (param "WRITE_DIR")))
(allow file-write* (subpath (param "WRITE_DIR_ALT")))
(allow file-write* (subpath "/private/var/folders"))
(deny network*)"#;

#[cfg(target_os = "macos")]
fn sbpl_literal(path: &str) -> String {
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(target_os = "macos")]
fn additional_host_read_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
    {
        if cargo_home.exists() {
            roots.push(cargo_home);
        }
    }
    if let Some(rustup_home) = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")))
    {
        if rustup_home.exists() {
            roots.push(rustup_home);
        }
    }
    roots
}

#[cfg(target_os = "macos")]
fn sandboxed_command(program: &str, sandbox_root: &Path) -> Command {
    let mut profile = String::from(SEATBELT_PROFILE_BASE);
    let program_path = PathBuf::from(program);
    if program_path.is_absolute() {
        let program_text = program_path.to_string_lossy();
        profile.push('\n');
        profile.push_str("(allow process-exec (literal ");
        profile.push_str(&sbpl_literal(&program_text));
        profile.push_str("))");
        profile.push('\n');
        profile.push_str("(allow file-read* (literal ");
        profile.push_str(&sbpl_literal(&program_text));
        profile.push_str("))");
    }
    for root in additional_host_read_roots() {
        let root_text = root.to_string_lossy();
        profile.push('\n');
        profile.push_str("(allow file-read* (subpath ");
        profile.push_str(&sbpl_literal(&root_text));
        profile.push_str("))");
    }

    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.arg("-p").arg(profile);
    let write_dir = sandbox_root.to_string_lossy().into_owned();
    let write_dir_alt = write_dir.replace("/tmp", "/private/tmp");
    command.arg("-D").arg(format!("WRITE_DIR={write_dir}"));
    command
        .arg("-D")
        .arg(format!("WRITE_DIR_ALT={write_dir_alt}"));
    command.arg("-D").arg(format!("VFS_ROOT={write_dir}"));
    command
        .arg("-D")
        .arg(format!("VFS_ROOT_ALT={write_dir_alt}"));
    command.arg(program);
    command
}

#[cfg(not(target_os = "macos"))]
fn sandboxed_command(program: &str, _sandbox_root: &Path) -> Command {
    Command::new(program)
}
