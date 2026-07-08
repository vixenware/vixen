#![cfg(target_os = "macos")]

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use camino::Utf8PathBuf;
use vx_exec::FskitVfsExecutor;
use vx_services::{
    ByteLen, ExecArg, ExecArgValue, ExecBlake3Hash as Blake3Hash, ExecCompletion,
    ExecContentHash as ContentHash, ExecExitCode, ExecMount, ExecOutcome, ExecPath, ExecPlan,
    ExecProgram, ExecReadObservation, ExecRequest, ExecRole, ExecText, ExecTree, Runner,
    StoreClient, StoreDispatcher,
};
use vx_vfsd::protocol::{Vfsd, VfsdDispatcher};

const DEFAULT_NATIVE_VFS_MOUNT: &str = "/Volumes/Vixen/vfs";
const NATIVE_VFS_TEST_LOCK_PATH: &str = "/tmp/vx-exec-native-vfs-test.lock";

#[tokio::test]
async fn native_vfs_exec_observes_file_directory_and_lookup_miss() {
    if std::env::var_os("VX_EXEC_NATIVE_VFS_TEST").is_none() || !host_tools_available() {
        return;
    }
    let _guard = NativeVfsTestLock::lock();
    let harness = NativeVfsHarness::boot().await;

    let mut input_tree = ExecTree::default();
    input_tree.insert_bytes("a.txt", b"alpha\n".to_vec());
    input_tree.insert_bytes("b.txt", b"beta\n".to_vec());

    let result = harness
        .executor()
        .exec(ExecRequest {
            program: ExecProgram::from("/bin/sh"),
            plan: ExecPlan {
                argv: vec![
                    arg("-c", ExecRole::Flag),
                    arg(
                        "set -e; cat \"$1\" >/dev/null; cat \"$B\" >/dev/null; ls \"$DIR\" >/dev/null; test ! -e \"$MISS\"; printf 'observed\\n' > \"$OUT\"",
                        ExecRole::Flag,
                    ),
                    arg("sh", ExecRole::Flag),
                    arg("/m/in/a.txt", ExecRole::Input),
                    arg("B=/m/in/b.txt", ExecRole::Env),
                    arg("DIR=/m/in", ExecRole::Env),
                    arg("MISS=/m/in/missing.txt", ExecRole::Env),
                    arg("OUT=/m/out/combined.txt", ExecRole::Env),
                    arg("/m/out/combined.txt", ExecRole::Output),
                ],
            },
            mounts: vec![ExecMount {
                at: ExecPath::from("/m/in"),
                tree: input_tree,
            }],
            toolchain_roots: default_toolchain_roots(),
        })
        .await;

    assert_eq!(
        result.exit_code,
        ExecExitCode(0),
        "error: {:?}\nstderr: {}",
        failure(&result),
        stderr(&result)
    );
    let outcome = outcome(&result);
    assert_eq!(
        outcome.outputs.entries.get("/m/out/combined.txt"),
        Some(&ExecText::from("observed\n"))
    );
    assert!(matches!(
        outcome.read_set.entries.get("/m/in/a.txt"),
        Some(ExecReadObservation::File { content_hash, size, .. })
            if content_hash == &ContentHash(Blake3Hash::from_bytes(b"alpha\n"))
                && size == &ByteLen(6)
    ));
    assert!(matches!(
        outcome.read_set.entries.get("/m/in/b.txt"),
        Some(ExecReadObservation::File { content_hash, size, .. })
            if content_hash == &ContentHash(Blake3Hash::from_bytes(b"beta\n"))
                && size == &ByteLen(5)
    ));
    assert!(matches!(
        outcome.read_set.entries.get("/m/in"),
        Some(ExecReadObservation::Directory { .. })
    ));
    assert!(matches!(
        outcome.read_set.entries.get("/m/in/missing.txt"),
        Some(ExecReadObservation::LookupMiss { parent_path, .. }) if parent_path == &ExecPath::from("/m/in")
    ));
}

#[tokio::test]
async fn native_vfs_build_script_executes_cas_executable() {
    if std::env::var_os("VX_EXEC_NATIVE_VFS_TEST").is_none() || !host_tools_available() {
        return;
    }
    let _guard = NativeVfsTestLock::lock();
    let harness = NativeVfsHarness::boot().await;

    let mut input_tree = ExecTree::default();
    input_tree.insert_bytes(
        "runner",
        b"#!/bin/sh\nset -e\ncat \"$IN\" >/dev/null\nprintf 'exec-ok\\n' > \"$OUT\"\n".to_vec(),
    );
    input_tree.insert_bytes("data.txt", b"payload\n".to_vec());

    let result = harness
        .executor()
        .exec(ExecRequest {
            program: ExecProgram::from("build_script"),
            plan: ExecPlan {
                argv: vec![
                    arg("/m/in/runner", ExecRole::Executable),
                    arg("IN=/m/in/data.txt", ExecRole::Env),
                    arg("OUT=/m/out/result.txt", ExecRole::Env),
                    arg("/m/in/data.txt", ExecRole::Input),
                    arg("/m/out/result.txt", ExecRole::Output),
                ],
            },
            mounts: vec![ExecMount {
                at: ExecPath::from("/m/in"),
                tree: input_tree,
            }],
            toolchain_roots: default_toolchain_roots(),
        })
        .await;

    assert_eq!(
        result.exit_code,
        ExecExitCode(0),
        "error: {:?}\nstderr: {}",
        failure(&result),
        stderr(&result)
    );
    let outcome = outcome(&result);
    assert_eq!(
        outcome.outputs.entries.get("/m/out/result.txt"),
        Some(&ExecText::from("exec-ok\n"))
    );
    assert!(matches!(
        outcome.read_set.entries.get("/m/in/runner"),
        Some(ExecReadObservation::File { .. })
    ));
}

struct NativeVfsHarness {
    vfsd_addr: String,
    store_addr: String,
    access_path: Utf8PathBuf,
    vfsd_server: tokio::task::JoinHandle<()>,
    store_server: tokio::task::JoinHandle<()>,
}

impl NativeVfsHarness {
    async fn boot() -> Self {
        let mount_path = std::env::var("VX_EXEC_NATIVE_VFS_MOUNT")
            .unwrap_or_else(|_| DEFAULT_NATIVE_VFS_MOUNT.into());
        let mount = Utf8PathBuf::from(mount_path);
        assert!(
            mount.as_std_path().exists(),
            "live VixenFS mount is missing at {mount}"
        );

        let store_root = tempfile::tempdir().expect("store tempdir");
        let store =
            vx_store::open(Utf8PathBuf::from_path_buf(store_root.path().to_path_buf()).unwrap())
                .await
                .expect("open store");
        let (store_addr, store_server) = spawn_store_service(store.clone()).await;
        let cas: StoreClient = vox::connect_lane(&store_addr)
            .connect_timeout(Duration::from_secs(1))
            .wait_for_service(Duration::from_secs(5))
            .await
            .expect("connect store client");
        let service = vx_vfsd::boot_with_store(Arc::new(cas), &mount)
            .await
            .expect("boot vx-vfsd provider");
        let access_path = vx_vfsd::effective_access_path(&mount);
        let (vfsd_addr, vfsd_server) = spawn_vfsd_service(service).await;

        Self {
            vfsd_addr,
            store_addr,
            access_path,
            vfsd_server,
            store_server,
        }
    }

    fn executor(&self) -> FskitVfsExecutor {
        FskitVfsExecutor::new(
            self.vfsd_addr.clone(),
            self.store_addr.clone(),
            self.access_path.as_std_path(),
        )
        .with_timeouts(Duration::from_secs(1), Duration::from_secs(5))
    }
}

impl Drop for NativeVfsHarness {
    fn drop(&mut self) {
        self.vfsd_server.abort();
        self.store_server.abort();
    }
}

async fn spawn_vfsd_service(service: impl Vfsd + 'static) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind vfsd test listener");
    let addr = listener.local_addr().expect("vfsd listener addr");
    let server = tokio::spawn(async move {
        let result = vox::serve_listener(listener, VfsdDispatcher::new(service)).await;
        if let Err(error) = result {
            tracing::debug!(?error, "vfsd test listener stopped");
        }
    });
    (format!("tcp://{addr}"), server)
}

async fn spawn_store_service(
    service: vx_store::StoreService,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind store test listener");
    let addr = listener.local_addr().expect("store listener addr");
    let server = tokio::spawn(async move {
        let result = vox::serve_listener(listener, StoreDispatcher::new(service)).await;
        if let Err(error) = result {
            tracing::debug!(?error, "store test listener stopped");
        }
    });
    (format!("tcp://{addr}"), server)
}

fn arg(value: &str, role: ExecRole) -> ExecArg {
    ExecArg {
        value: ExecArgValue::from(value),
        role,
    }
}

fn default_toolchain_roots() -> Vec<ExecPath> {
    vec![ExecPath::from("/bin"), ExecPath::from("/usr/bin")]
}

fn host_tools_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists() && Path::new("/bin/sh").exists()
}

struct NativeVfsTestLock {
    file: File,
}

impl NativeVfsTestLock {
    fn lock() -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(NATIVE_VFS_TEST_LOCK_PATH)
            .expect("open native vfs test lock");
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(rc, 0, "flock native vfs test lock");
        Self { file }
    }
}

impl Drop for NativeVfsTestLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn stderr(result: &vx_services::ExecResult) -> String {
    String::from_utf8_lossy(&result.stderr).to_string()
}

fn outcome(result: &vx_services::ExecResult) -> &ExecOutcome {
    match &result.completion {
        ExecCompletion::Succeeded(outcome) => outcome,
        ExecCompletion::Failed(error) => panic!("expected success, got {error:?}"),
    }
}

fn failure(result: &vx_services::ExecResult) -> Option<&vx_services::ExecError> {
    match &result.completion {
        ExecCompletion::Failed(error) => Some(error),
        ExecCompletion::Succeeded(_) => None,
    }
}
