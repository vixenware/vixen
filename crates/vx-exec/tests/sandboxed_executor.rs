#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::Utf8PathBuf;
use vx_exec::{
    ExecObservationFeed, ExecObservationFuture, FskitObservationFeed, FskitVfsExecutor,
    SandboxedExecutor, vfs_tracked_observations_to_exec_read_set,
};
use vx_services::{
    ByteLen, ExecArg, ExecArgValue, ExecBlake3Hash as Blake3Hash, ExecCompletion,
    ExecContentHash as ContentHash, ExecError, ExecErrorKind, ExecExitCode, ExecMount, ExecOutcome,
    ExecPath, ExecPlan, ExecPrefixId, ExecProgram, ExecReadObservation, ExecReadSet, ExecRequest,
    ExecRole, ExecText, ExecTree, ExecTreeEvent, Runner, StoreClient, StoreDispatcher,
};
use vx_vfsd::protocol::{VfsPrefixStateSummary, VfsTrackedObservation, Vfsd, VfsdDispatcher};

#[tokio::test]
async fn shell_exec_harvests_declared_output() {
    if !host_tools_available() {
        return;
    }

    let mut input_tree = ExecTree::default();
    input_tree.insert_bytes("message.txt", b"hello from declared mount\n".to_vec());

    let result = SandboxedExecutor::new()
        .exec(ExecRequest {
            program: ExecProgram::from("/bin/sh"),
            plan: ExecPlan {
                argv: vec![
                    arg("-c", ExecRole::Flag),
                    arg("cat \"$1\" > \"$OUT\"", ExecRole::Flag),
                    arg("sh", ExecRole::Flag),
                    arg("/m/in/message.txt", ExecRole::Input),
                    arg("OUT=/m/out/hello.txt", ExecRole::Env),
                    arg("/m/out/hello.txt", ExecRole::Output),
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
        outcome.outputs.entries.get("/m/out/hello.txt"),
        Some(&ExecText::from("hello from declared mount\n"))
    );
    assert_eq!(
        outcome.read_set.entries.get("/m/in/message.txt"),
        Some(&ExecReadObservation::File {
            content_hash: ContentHash(Blake3Hash::from_bytes(b"hello from declared mount\n")),
            blob_node: None,
            size: ByteLen("hello from declared mount\n".len() as u64),
        })
    );
    assert_eq!(outcome.observation_scopes.len(), 1);
    assert_eq!(
        outcome.observation_scopes[0].roots,
        vec![ExecPath::from("/m/in")]
    );
    assert!(
        outcome
            .tree_events
            .iter()
            .any(|event| matches!(event, ExecTreeEvent::SubfileCompleted(file) if file.path == ExecPath::from("/m/out/hello.txt")))
    );
    assert!(matches!(
        outcome.tree_events.last(),
        Some(ExecTreeEvent::TreeFinalized(finalization)) if finalization.files.len() == 1
    ));
}

#[tokio::test]
async fn undeclared_read_is_loud_failure() {
    if !host_tools_available() {
        return;
    }

    let fixture = Fixture::new("undeclared-read");
    let secret = fixture.root.join("secret.txt");
    fs::write(&secret, "not declared\n").unwrap();

    let result = SandboxedExecutor::new()
        .exec(shell_request(
            &format!("cat {} > \"$OUT\"", shell_quote(&secret)),
            "/m/out/read.txt",
        ))
        .await;

    assert_ne!(
        result.exit_code,
        ExecExitCode(0),
        "undeclared read unexpectedly succeeded"
    );
    let error = failure(&result);
    assert_ne!(error.kind, ExecErrorKind::Protocol);
    assert!(
        error.kind == ExecErrorKind::ProcessExit || error.kind == ExecErrorKind::Spawn,
        "failure should be machine-visible: {:?}\nstderr:\n{}",
        error,
        stderr(&result)
    );
}

#[tokio::test]
async fn observed_read_set_contains_files_directory_and_lookup_miss() {
    if !host_tools_available() {
        return;
    }

    let mut input_tree = ExecTree::default();
    input_tree.insert_bytes("a.txt", b"alpha\n".to_vec());
    input_tree.insert_bytes("b.txt", b"beta\n".to_vec());

    let result = SandboxedExecutor::new()
        .exec(ExecRequest {
            program: ExecProgram::from("/bin/sh"),
            plan: ExecPlan {
                argv: vec![
                    arg("-c", ExecRole::Flag),
                    arg(
                        "set -e; cat \"$1\" >/dev/null; cat \"$B\" >/dev/null; printf 'observed\\n' > \"$OUT\"",
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
    assert_eq!(outcome.read_set.entries.len(), 4);
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
async fn observation_feed_supplies_exact_read_set() {
    if !host_tools_available() {
        return;
    }

    let mut input_tree = ExecTree::default();
    input_tree.insert_bytes(
        "message.txt",
        b"staged but not the read-set source\n".to_vec(),
    );

    let mut feed_read_set = ExecReadSet::default();
    feed_read_set.entries.insert(
        ExecPath::from("/m/in/message.txt"),
        ExecReadObservation::File {
            content_hash: ContentHash(Blake3Hash::from_bytes(b"observed by fskit feed\n")),
            blob_node: None,
            size: ByteLen("observed by fskit feed\n".len() as u64),
        },
    );
    feed_read_set.entries.insert(
        ExecPath::from("/m/in"),
        ExecReadObservation::Directory {
            directory_node: vx_services::ExecNodeHash(Blake3Hash::from_bytes(
                b"feed directory node",
            )),
        },
    );
    feed_read_set.entries.insert(
        ExecPath::from("/m/in/missing.h"),
        ExecReadObservation::LookupMiss {
            parent_path: ExecPath::from("/m/in"),
            directory_node: vx_services::ExecNodeHash(Blake3Hash::from_bytes(
                b"feed lookup miss directory",
            )),
        },
    );

    let feed = InMemoryObservationFeed::new(feed_read_set.clone());
    let result = SandboxedExecutor::with_observation_feed(feed.clone())
        .exec(ExecRequest {
            program: ExecProgram::from("/bin/sh"),
            plan: ExecPlan {
                argv: vec![
                    arg("-c", ExecRole::Flag),
                    arg("cat \"$1\" > \"$OUT\"", ExecRole::Flag),
                    arg("sh", ExecRole::Flag),
                    arg("/m/in/message.txt", ExecRole::Input),
                    arg("OUT=/m/out/hello.txt", ExecRole::Env),
                    arg("/m/out/hello.txt", ExecRole::Output),
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
    assert_eq!(outcome.read_set, feed_read_set);
    assert_eq!(
        outcome.outputs.entries.get("/m/out/hello.txt"),
        Some(&ExecText::from("staged but not the read-set source\n"))
    );
    let requests = feed.requests().expect("observation feed requests");
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].0.0.starts_with("vx-sandboxed-exec-"),
        "unexpected observation prefix id: {:?}",
        requests[0].0
    );
    assert_eq!(requests[0].1, vec![ExecPath::from("/m/in")]);
}

#[tokio::test]
async fn fskit_observation_feed_calls_vfsd_with_prefix_relative_roots() {
    let file_hash = vx_services::Blake3Hash::from_bytes(b"alpha\n");
    let directory_node = vx_services::NodeHash(vx_services::Blake3Hash::from_bytes(b"dir-node"));
    let observations = vec![
        VfsTrackedObservation::File {
            path: "m/in/a.txt".to_string(),
            content_hash: file_hash,
            blob_node: None,
            size: 6,
        },
        VfsTrackedObservation::Directory {
            path: "m/in".to_string(),
            directory_node,
        },
        VfsTrackedObservation::LookupMiss {
            path: "m/in/missing.txt".to_string(),
            parent_path: "m/in".to_string(),
            directory_node,
        },
    ];
    let service = FakeVfsd::new(observations);
    let calls = service.calls.clone();
    let (addr, server) = spawn_vfsd_service(service).await;

    let feed = FskitObservationFeed::new(addr)
        .with_timeouts(Duration::from_secs(1), Duration::from_secs(1));
    let read_set = feed
        .read_set(
            &ExecPrefixId("exec-123".to_string()),
            &[ExecPath::from("/m/in")],
        )
        .await
        .expect("feed read-set")
        .expect("fskit feed is authoritative");

    server.abort();
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[("exec-123".to_string(), vec!["m/in".to_string()])]
    );
    assert_eq!(
        read_set.entries.get("/m/in/a.txt"),
        Some(&ExecReadObservation::File {
            content_hash: ContentHash(Blake3Hash(file_hash.0)),
            blob_node: None,
            size: ByteLen(6),
        })
    );
    assert!(matches!(
        read_set.entries.get("/m/in"),
        Some(ExecReadObservation::Directory { .. })
    ));
    assert!(matches!(
        read_set.entries.get("/m/in/missing.txt"),
        Some(ExecReadObservation::LookupMiss { parent_path, .. }) if parent_path == &ExecPath::from("/m/in")
    ));
}

#[test]
fn fskit_observation_mapping_adds_exec_leading_slash() {
    let file_hash = vx_services::Blake3Hash::from_bytes(b"alpha\n");
    let blob_node = vx_services::NodeHash(vx_services::Blake3Hash::from_bytes(b"blob-node"));
    let directory_node = vx_services::NodeHash(vx_services::Blake3Hash::from_bytes(b"dir-node"));

    let read_set = vfs_tracked_observations_to_exec_read_set(vec![
        VfsTrackedObservation::File {
            path: "m/in/a.txt".to_string(),
            content_hash: file_hash,
            blob_node: Some(blob_node),
            size: 6,
        },
        VfsTrackedObservation::Directory {
            path: "m/in".to_string(),
            directory_node,
        },
        VfsTrackedObservation::LookupMiss {
            path: "m/in/missing.txt".to_string(),
            parent_path: "m/in".to_string(),
            directory_node,
        },
    ]);

    assert_eq!(
        read_set.entries.get("/m/in/a.txt"),
        Some(&ExecReadObservation::File {
            content_hash: ContentHash(Blake3Hash(file_hash.0)),
            blob_node: Some(vx_services::ExecNodeHash(Blake3Hash(blob_node.0.0))),
            size: ByteLen(6),
        })
    );
    assert!(read_set.entries.contains_key("/m/in"));
    assert!(matches!(
        read_set.entries.get("/m/in/missing.txt"),
        Some(ExecReadObservation::LookupMiss { parent_path, directory_node: node })
            if parent_path == &ExecPath::from("/m/in")
                && node == &vx_services::ExecNodeHash(Blake3Hash(directory_node.0.0))
    ));
}

#[tokio::test]
#[ignore = "requires live /Volumes/Vixen/vfs and the FSKit extension"]
async fn live_fskit_vfs_executor_uses_tracked_observations_as_read_set() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();
    if !host_tools_available() {
        return;
    }
    let mount = Utf8PathBuf::from(vx_vfs_types::MACOS_VFS_MOUNT_POINT);
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
        .expect("boot singleton vx-vfsd provider");
    let access_path = vx_vfsd::effective_access_path(&mount);
    let (vfsd_addr, vfsd_server) = spawn_vfsd_service(service).await;

    let mut input_tree = ExecTree::default();
    input_tree.insert_bytes("message.txt", b"hello from live fskit\n".to_vec());
    let result = FskitVfsExecutor::new(
        vfsd_addr.clone(),
        store_addr.clone(),
        access_path.as_std_path(),
    )
    .with_timeouts(Duration::from_secs(1), Duration::from_secs(5))
    .exec(ExecRequest {
        program: ExecProgram::from("/bin/cat"),
        plan: ExecPlan {
            argv: vec![
                arg("/m/in/message.txt", ExecRole::Input),
                arg("/m/out/live.txt", ExecRole::Stdout),
            ],
        },
        mounts: vec![ExecMount {
            at: ExecPath::from("/m/in"),
            tree: input_tree,
        }],
        toolchain_roots: default_toolchain_roots(),
    })
    .await;

    vfsd_server.abort();
    store_server.abort();

    assert_eq!(
        result.exit_code,
        ExecExitCode(0),
        "error: {:?}\nstderr: {}",
        failure(&result),
        stderr(&result)
    );
    let outcome = outcome(&result);
    tracing::info!(read_set = ?outcome.read_set, "live fskit observed read-set");
    assert_eq!(
        outcome.outputs.entries.get("/m/out/live.txt"),
        Some(&ExecText::from("hello from live fskit\n"))
    );
    assert_eq!(
        outcome.read_set.entries.get("/m/in/message.txt"),
        Some(&ExecReadObservation::File {
            content_hash: ContentHash(Blake3Hash::from_bytes(b"hello from live fskit\n")),
            blob_node: None,
            size: ByteLen("hello from live fskit\n".len() as u64),
        })
    );
}

#[tokio::test]
async fn undeclared_write_is_denied() {
    if !host_tools_available() {
        return;
    }

    let fixture = Fixture::new("undeclared-write");
    let outside = fixture.root.join("outside.txt");

    let result = SandboxedExecutor::new()
        .exec(shell_request(
            &format!("printf 'not allowed\n' > {}", shell_quote(&outside)),
            "/m/out/write.txt",
        ))
        .await;

    assert_ne!(
        result.exit_code,
        ExecExitCode(0),
        "undeclared write unexpectedly succeeded"
    );
    let error = failure(&result);
    assert!(!outside.exists(), "sandbox denial must prevent the write");
    assert!(
        error.kind == ExecErrorKind::ProcessExit || error.kind == ExecErrorKind::Spawn,
        "failure should be machine-visible\nstderr:\n{}",
        stderr(&result)
    );
}

fn shell_request(script: &str, output: &str) -> ExecRequest {
    ExecRequest {
        program: ExecProgram::from("/bin/sh"),
        plan: ExecPlan {
            argv: vec![
                ExecArg {
                    value: ExecArgValue::from("-c"),
                    role: ExecRole::Flag,
                },
                ExecArg {
                    value: ExecArgValue::from(script),
                    role: ExecRole::Flag,
                },
                ExecArg {
                    value: ExecArgValue::from(format!("OUT={output}")),
                    role: ExecRole::Env,
                },
                ExecArg {
                    value: ExecArgValue::from(output),
                    role: ExecRole::Output,
                },
            ],
        },
        mounts: Vec::new(),
        toolchain_roots: default_toolchain_roots(),
    }
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

fn stderr(result: &vx_services::ExecResult) -> String {
    String::from_utf8_lossy(&result.stderr).to_string()
}

#[derive(Clone)]
struct InMemoryObservationFeed {
    read_set: ExecReadSet,
    requests: Arc<Mutex<ObservationRequests>>,
}

type ObservationRequests = Vec<(ExecPrefixId, Vec<ExecPath>)>;

impl InMemoryObservationFeed {
    fn new(read_set: ExecReadSet) -> Self {
        Self {
            read_set,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Result<Vec<(ExecPrefixId, Vec<ExecPath>)>, String> {
        self.requests
            .lock()
            .map_err(|_| "observation feed requests poisoned".to_string())
            .map(|requests| requests.clone())
    }
}

impl ExecObservationFeed for InMemoryObservationFeed {
    fn read_set<'a>(
        &'a self,
        prefix_id: &'a ExecPrefixId,
        roots: &'a [ExecPath],
    ) -> ExecObservationFuture<'a> {
        Box::pin(async move {
            if roots.is_empty() {
                return Err(observation_error("missing observation root"));
            }
            self.requests
                .lock()
                .map_err(|_| observation_error("observation feed requests poisoned"))?
                .push((prefix_id.clone(), roots.to_vec()));
            Ok(Some(self.read_set.clone()))
        })
    }
}

#[derive(Clone)]
struct FakeVfsd {
    observations: Vec<VfsTrackedObservation>,
    calls: FakeVfsdCalls,
}

type FakeVfsdCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

impl FakeVfsd {
    fn new(observations: Vec<VfsTrackedObservation>) -> Self {
        Self {
            observations,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Vfsd for FakeVfsd {
    async fn mkdir(&self, _path: String) -> Result<(), String> {
        Ok(())
    }

    async fn mount(
        &self,
        _path: String,
        _tree_hash: vx_services::Blake3Hash,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn flush(&self, _path: String) -> Result<vx_services::Blake3Hash, String> {
        Ok(vx_services::Blake3Hash::from_bytes(b"fake flush"))
    }

    async fn rm_rf(&self, _path: String) -> Result<(), String> {
        Ok(())
    }

    async fn ping(&self) -> Result<(), String> {
        Ok(())
    }

    async fn dump_state(&self, _prefix_id: String) -> Result<(), String> {
        Ok(())
    }

    async fn access_path(&self) -> Result<String, String> {
        Ok("/Volumes/Vixen/vfs/vx".to_string())
    }

    async fn describe_prefix_state(
        &self,
        prefix_id: String,
    ) -> Result<VfsPrefixStateSummary, String> {
        Ok(VfsPrefixStateSummary {
            prefix_id,
            prefix_found: true,
            total_items: 0,
            total_prefixes: 1,
            path_mappings_count: 0,
            root_children: Vec::new(),
            prefix_children: Vec::new(),
            src_children: Vec::new(),
            toolchain_children: Vec::new(),
            toolchain_bin_children: Vec::new(),
            path_mappings_sample: Vec::new(),
            src_path_mappings_sample: Vec::new(),
        })
    }

    async fn tracked_observations(
        &self,
        prefix_id: String,
        roots: Vec<String>,
    ) -> Result<Vec<VfsTrackedObservation>, String> {
        self.calls
            .lock()
            .map_err(|_| "fake vfsd calls poisoned".to_string())?
            .push((prefix_id, roots));
        Ok(self.observations.clone())
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

fn observation_error(message: &str) -> ExecError {
    ExecError {
        kind: ExecErrorKind::Verification,
        diagnostic: vx_services::ExecDiagnostic(message.to_string()),
    }
}

fn outcome(result: &vx_services::ExecResult) -> &ExecOutcome {
    match &result.completion {
        ExecCompletion::Succeeded(outcome) => outcome,
        ExecCompletion::Failed(error) => panic!("expected success, got {error:?}"),
    }
}

fn failure(result: &vx_services::ExecResult) -> &ExecError {
    match &result.completion {
        ExecCompletion::Failed(error) => error,
        ExecCompletion::Succeeded(_) => panic!("expected failure"),
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "vx-exec-{name}-{}-{}",
            std::process::id(),
            monotonic_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn monotonic_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
