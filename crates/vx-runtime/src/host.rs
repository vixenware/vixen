//! The `Host` capability — a read-only view of the unsandboxed executor machine,
//! handed to a capability probe (r[exec.verifier.host]). Held at runtime as a
//! generic Value::Opaque, never a dedicated Value variant. Host paths are real-FS
//! `String`s, not VFS `Path`s.

use std::path::Path;

/// A handle to the local host machine. A local host carries no state yet; a
/// remote/region-scoped host would carry its executor identity here. Held at
/// runtime inside a [`crate::eval::Value::Opaque`].
#[derive(Debug)]
pub struct HostHandle;

impl HostHandle {
    /// The local (in-process) host.
    pub fn local() -> Self {
        HostHandle
    }
}

impl crate::eval::Opaque for HostHandle {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
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

/// Hash a host file with Blake3, returning the lowercase hex digest.
pub fn hash(path: &str) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Read a host file's raw bytes.
pub fn read(path: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(path)
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
