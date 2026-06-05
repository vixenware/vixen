//! End-to-end proof of the exec spine on the **live** CAS-VFS.
//!
//! Spawns a real `vixen daemon` (store + vfsd + runner), then drives the runner's
//! `exec` over vox: run `sh -c 'printf … > out/y.txt'` with `out/` declared as a
//! writable root. Asserts the process succeeded, then **mounts the flushed write-set
//! tree and reads `y.txt` back** — proving sandboxed run → flush → tree → mount → read
//! round-trips. (Mounting a CAS *input* tree is a focused follow-on.)
//!
//! Gated `#[ignore]`: it needs the live FSKit mount (the Vixen mac extension), which
//! isn't present in a bare CI/sandbox. Run on a host with the VFS:
//!     cargo test -p vx --test exec_e2e -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use tempfile::TempDir;
use vx_services::{ExecRequest, RunnerClient};
use vx_vfsd::protocol::VfsdClient;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn vixen_bin() -> String {
    std::env::var("CARGO_BIN_EXE_vixen").expect("vixen binary built for integration tests")
}

/// A daemon config with store + vfsd + runner enabled.
fn write_config(temp: &TempDir, sock: &Path, store: &Path, vfs_mount: &str) -> PathBuf {
    let cfg = temp.path().join("daemon.styx");
    std::fs::write(
        &cfg,
        format!(
            "daemon {{\n  listen (\"local://{}\")\n  services {{\n    store {{ root \"{}\" }}\n    vfsd {{ mount \"{}\" }}\n    runner {{}}\n  }}\n}}\n",
            sock.display(),
            store.display(),
            vfs_mount,
        ),
    )
    .expect("write daemon config");
    cfg
}

fn start_daemon(config: &Path) -> Child {
    Command::new(vixen_bin())
        .current_dir(workspace_root())
        .args(["daemon", "--config"])
        .arg(config)
        .spawn()
        .expect("spawn vixen daemon")
}

#[tokio::test]
#[ignore = "needs the live FSKit VFS (the Vixen mac extension); run with --ignored on a host with the mount"]
async fn exec_runs_against_the_vfs_and_flushes_the_write_set() {
    let temp = TempDir::new().expect("temp dir");
    let sock = temp.path().join("vx.sock");
    let store_root = temp.path().join("store");
    // The default single-instance mount. `vixen://session` is *one* FSKit resource:
    // a second `mount` of it (e.g. at a per-run path) is refused with "resource busy"
    // (`canStartTask … resource state is 5`). The daemon reads status.json and *reuses*
    // a live extension, so repeated runs share this one mount — that reuse is the
    // resilience strategy, not a hazard. (A /tmp subdirectory, never top-level.)
    let vfs_mount = "/tmp/vx/vfs";
    let config = write_config(&temp, &sock, &store_root, vfs_mount);
    let mut daemon = start_daemon(&config);

    let addr = format!("local://{}", sock.display());

    // Wait for the daemon (and its VFS) to come up — retry the first connect.
    let mut connected: Option<RunnerClient> = None;
    for _ in 0..100 {
        if let Ok(client) = vox::connect(&addr).await {
            connected = Some(client);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let runner = connected.expect("daemon did not become reachable");

    // Run a process that writes into the declared output root.
    let result = runner
        .exec(ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf payload-bytes > out/y.txt".to_string(),
            ],
            env: Vec::new(),
            mounts: Vec::new(),
            outputs: vec!["out".to_string()],
            create_dirs: Vec::new(),
        })
        .await
        .expect("runner.exec RPC");

    assert!(
        result.error.is_none(),
        "exec should run cleanly, got error: {:?} (stderr: {})",
        result.error,
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.exit_code, 0, "the process should exit 0");
    assert_eq!(result.outputs.len(), 1, "one flushed output root");
    let out_tree = result.outputs[0].tree;

    // Mount the flushed write-set and read `y.txt` back — the round-trip proof.
    let vfsd: VfsdClient = vox::connect(&addr).await.expect("connect vfsd");
    let _ = vfsd.rm_rf("/verify".to_string()).await;
    vfsd.mount("/verify".to_string(), out_tree)
        .await
        .expect("mount the flushed tree");
    let access = vfsd.access_path().await.expect("access path");
    let y = format!("{}/verify/y.txt", access.trim_end_matches('/'));
    let got = std::fs::read(&y).unwrap_or_else(|e| panic!("read {y}: {e}"));
    assert_eq!(
        got, b"payload-bytes",
        "the flushed output should be exactly what the process wrote from the mounted input"
    );

    let _ = vfsd.rm_rf("/verify".to_string()).await;
    let _ = daemon.kill();
    let _ = daemon.wait();
}
