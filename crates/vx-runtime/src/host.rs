//! The `Host` capability — a read-only view of the unsandboxed executor machine,
//! handed to a capability probe (r[exec.verifier.host]). `Host` is an ordinary
//! struct (`struct Host { os: String, arch: String }`): `os`/`arch` are plain
//! fields filled by Rust, and being Rust-backed is just an implementation detail
//! of its methods — it earns no special `Value` representation. Host paths are
//! real-FS `String`s, not VFS `Path`s.

use std::path::Path;

use crate::eval::{Thunk, Value};

/// Build the local host as an ordinary struct value: `{ os, arch }`, each field a
/// `Value::Str` filled from the compiled-in target constants. This is what a probe
/// receives as its `Host` argument; the struct shape means `host.os` is a plain
/// field read and `host.which(name)` is a name-based method dispatch (see
/// `eval::call_method`), with no dedicated `Value` variant.
pub fn local_host_value() -> Value {
    Value::Struct(vec![
        (
            "os".to_string(),
            Thunk::done(Value::Str(std::env::consts::OS.into())),
        ),
        (
            "arch".to_string(),
            Thunk::done(Value::Str(std::env::consts::ARCH.into())),
        ),
    ])
}

/// Resolve a tool name against `$PATH`, returning the absolute path of the first
/// executable file named `name`. `None` if no entry resolves (the runtime turns
/// that into a poison — the failure model's absent-tool case).
pub fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            // Resolve to an absolute path (a `$PATH` entry may be relative).
            return std::fs::canonicalize(&candidate)
                .ok()
                .and_then(|p| p.to_str().map(str::to_owned))
                .or_else(|| candidate.to_str().map(str::to_owned));
        }
    }
    None
}

/// What `host.exec` observed: the captured output + exit code, plus the
/// **fingerprint** of the binary that ran. `host.exec` *implies `host.hash`* —
/// running `args[0]` depends on its exact bytes, so we hash it as part of the same
/// observation (one call yields the tool's version *and* its content identity).
#[derive(facet::Facet, Clone, Debug, PartialEq, Eq)]
pub struct HostExec {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
    /// `blake3:<hex>` of the binary at `args[0]` — the probe puts this on the
    /// `Command` it builds, so the toolchain is content-identified, not path-keyed.
    pub fingerprint: String,
}

/// Run a discovered tool and observe it. `args[0]` is the program (an absolute path,
/// typically from `which`); the rest are its argv. Blake3-hashes the binary first
/// (`exec` implies `hash`), then runs it, returning the captured output + exit code
/// alongside that `blake3:…` fingerprint. Blocking — a probe runs to completion.
pub fn exec(args: &[String]) -> Result<HostExec, String> {
    let program = args
        .first()
        .ok_or_else(|| "host.exec needs a program".to_string())?;
    let fingerprint = fingerprint_executable(program)?;
    exec_with_fingerprint(args, fingerprint)
}

/// Content identity for the executable that [`std::process::Command`] will run.
pub fn fingerprint_executable(program: &str) -> Result<String, String> {
    let path = Path::new(program);
    let resolved = if path.components().count() == 1 {
        which(program).ok_or_else(|| format!("tool `{program}` not found on PATH"))?
    } else {
        program.to_string()
    };
    fingerprint_program(&resolved)
}

/// Content identity for a program that is about to cross a host execution boundary.
pub fn fingerprint_program(program: &str) -> Result<String, String> {
    let bytes = std::fs::read(program).map_err(|e| format!("read `{program}`: {e}"))?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

/// Run `args` with a fingerprint that was computed before the process was spawned.
pub fn exec_with_fingerprint(args: &[String], fingerprint: String) -> Result<HostExec, String> {
    let program = args
        .first()
        .ok_or_else(|| "host.exec needs a program".to_string())?;
    let out = std::process::Command::new(program)
        .args(&args[1..])
        .output()
        .map_err(|e| format!("exec `{program}`: {e}"))?;
    Ok(HostExec {
        exit_code: out.status.code().map(i64::from).unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        fingerprint,
    })
}

/// Whether `p` is a regular file the current user can execute. On Unix this checks
/// any execute bit; elsewhere a regular file is treated as runnable.
fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
