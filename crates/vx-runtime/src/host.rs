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
