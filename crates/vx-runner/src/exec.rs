//! The executor-side mount → run → flush spine, driving the working CAS-VFS.
//!
//! An action arrives as an [`ExecRequest`] (program + args + env, content-addressed
//! input mounts, writable output roots). We give it a fresh VFS prefix, mount the
//! inputs as trees, run the process **against the VFS access path** (the real
//! filesystem path the FSKit/FUSE mount is visible at), then flush each output root
//! back to a CAS tree — the write-set. The seatbelt/landlock sandbox wraps the spawn
//! next (the VFS already bounds the *inputs*; the sandbox bounds what else the
//! process may touch); caching rides on the flushed trees later.

use std::sync::atomic::{AtomicU64, Ordering};

use vx_services::{ExecOutput, ExecRequest, ExecResult};
use vx_vfsd::VfsdService;
use vx_vfsd::protocol::Vfsd;

/// A per-process action counter, so concurrent actions get distinct VFS prefixes.
static ACTION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Run one action to completion against the local `vfsd`, returning its outcome +
/// write-set. The runner is reached over vox, but drives its *own* CAS-VFS directly
/// (the in-process executor). A failure to *run the action* (mount, spawn, flush) is
/// reported in [`ExecResult::error`]; the process running and exiting nonzero is a
/// clean result with that exit code.
pub async fn run_exec(vfsd: &VfsdService, req: &ExecRequest) -> ExecResult {
    match run_exec_inner(vfsd, req).await {
        Ok(result) => result,
        Err(error) => ExecResult {
            exit_code: -1,
            stdout: Vec::new(),
            stderr: Vec::new(),
            outputs: Vec::new(),
            error: Some(error),
        },
    }
}

async fn run_exec_inner(vfsd: &VfsdService, req: &ExecRequest) -> Result<ExecResult, String> {
    // A fresh, unique prefix for this action (pid + a per-process counter).
    let prefix = format!(
        "/exec-{}-{}",
        std::process::id(),
        ACTION_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let _ = vfsd.rm_rf(prefix.clone()).await; // best-effort clean slate
    vfsd.mkdir(prefix.clone()).await?;

    // Mount each content-addressed input under the prefix.
    for mount in &req.mounts {
        let at = join(&prefix, &mount.path);
        mkdir_parents(vfsd, &at).await?;
        vfsd.mount(at.clone(), mount.tree)
            .await
            .map_err(|e| format!("mount {at}: {e}"))?;
    }
    // Create the writable output roots before launch.
    for out in &req.outputs {
        let at = join(&prefix, out);
        mkdir_parents(vfsd, &at).await?;
        vfsd.mkdir(at.clone())
            .await
            .map_err(|e| format!("mkdir output {at}: {e}"))?;
    }

    // The real filesystem path the VFS is mounted at — the process runs there, so it
    // sees the mounted inputs and writes into the output roots.
    let access = vfsd
        .access_path()
        .await
        .map_err(|e| format!("vfs access path: {e}"))?;
    let cwd = format!("{}{}", access.trim_end_matches('/'), prefix);

    // The real (canonicalized) output directories — the sandbox confines writes to
    // these (macOS resolves symlinks before the seatbelt check, so the unresolved
    // path would never match).
    let write_roots: Vec<String> = req
        .outputs
        .iter()
        .map(|out| {
            let dir = format!("{cwd}/{}", out.trim_matches('/'));
            std::fs::canonicalize(&dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(dir)
        })
        .collect();

    let output = crate::sandbox::make_command(&req.program, &req.args, &write_roots)
        .envs(req.env.iter().map(|e| (e.key.clone(), e.value.clone())))
        .current_dir(&cwd)
        .output()
        .await
        .map_err(|e| format!("spawn `{}`: {e}", req.program))?;

    // Flush each output root to a CAS tree — the write-set.
    let mut outputs = Vec::with_capacity(req.outputs.len());
    for out in &req.outputs {
        let at = join(&prefix, out);
        let tree = vfsd
            .flush(at.clone())
            .await
            .map_err(|e| format!("flush {at}: {e}"))?;
        outputs.push(ExecOutput {
            path: out.clone(),
            tree,
        });
    }

    let _ = vfsd.rm_rf(prefix).await; // best-effort cleanup (CAS blobs survive)

    Ok(ExecResult {
        exit_code: exit_code(&output.status),
        stdout: output.stdout,
        stderr: output.stderr,
        outputs,
        error: None,
    })
}

/// Join a VFS prefix (`/exec-…`) with a prefix-relative path, normalizing slashes.
fn join(prefix: &str, rel: &str) -> String {
    format!("{}/{}", prefix.trim_end_matches('/'), rel.trim_matches('/'))
}

/// `mkdir -p` the ancestors of `path` (the VFS `mkdir` needs the parent to exist).
async fn mkdir_parents(vfsd: &VfsdService, path: &str) -> Result<(), String> {
    let mut cur = String::new();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    // All but the last segment are ancestors to create.
    for seg in &segments[..segments.len().saturating_sub(1)] {
        cur.push('/');
        cur.push_str(seg);
        // Already-exists is fine — a sibling mount may have created it.
        let _ = vfsd.mkdir(cur.clone()).await;
    }
    Ok(())
}

/// The process exit status as a single integer: the exit code, or `128 + signal`
/// when terminated by a signal (the shell convention).
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(code) = status.code() {
            return code;
        }
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    #[cfg(not(unix))]
    {
        if let Some(code) = status.code() {
            return code;
        }
    }
    -1
}
