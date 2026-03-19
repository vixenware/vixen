use super::*;
use base64::Engine;
use flate2::Compression as GzipCompression;
use flate2::write::GzEncoder;
use moire::sync::SyncMutex;
use sha2::{Digest as _, Sha256};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use tar::Builder as TarBuilder;

use arborium::advanced::{CompiledGrammar, GrammarConfig};
use arborium::{GrammarStore, Highlighter};
use arborium_highlight::spans_to_themed;
use arborium_theme::theme::builtin;
use ariadne::{Color as AriadneColor, Config as AriadneConfig, Label, Report, ReportKind, Source};
use camino::{Utf8Path, Utf8PathBuf};
use concolor::ColorChoice;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use std::fs;
use std::fs::OpenOptions;
use std::net::TcpListener;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use tempfile::TempDir;
use tokio_test_lite::test as vx_test;
use vx_eval::{
    AcquireRequest as EvalAcquireRequest, BlobValue, DemandChain,
    DiscoverRequest as EvalDiscoverRequest, ExecArgValue, ExecPathValue, ExecRequest,
    ImportRequest as EvalImportRequest, MapValue, PathValue, RenderRequest as EvalRenderRequest,
    ShippedFunction, TransportBlock, TransportFnDef, TransportValue, TreeValue, Value,
};
use vx_io::boot::{BootDispatcher, BootGate, GracefulShutdown};
use vx_io::net::Listener;
use vx_io::spawn::{ServiceBuilder, ServiceHandle};
use vx_report::{GraphEdge, GraphNode, GraphTopology};
use vx_runner_proto::{
    DiscoverRequest, DiscoverResult, RUNNER_PROTOCOL_VERSION, RunMountMode, RunRequest, RunResult,
    Runner, RunnerClient, RunnerDispatcher,
};
use vx_store_proto::chunking::{prepare_blob, store_prepared_blob};
use vx_store_proto::{
    Blake3Hash, BlobRef, ContentHash, DirEntryKind, ServiceVersion, StoreClient, StoreProgress,
};
use vxd_proto::BuildRequest;

/// Create a fresh database for testing
fn test_db() -> Database {
    Database::new()
}

#[test]
fn parse_build_vx_executor_classes_accepts_local_daemon_and_ssh_bootstrap() {
    let parsed = crate::parse_build_vx_executor_classes(
        "default=local;linux=daemon:tcp://127.0.0.1:7777;windows=ssh_bootstrap:amos@winbox",
    )
    .expect("config should parse");

    assert_eq!(
        parsed,
        vec![
            crate::BuildVxExecutorClassConfig {
                class_label: "default".to_string(),
                backend: vx_eval::ExecutorBackendKind::Local,
                target: None,
            },
            crate::BuildVxExecutorClassConfig {
                class_label: "linux".to_string(),
                backend: vx_eval::ExecutorBackendKind::Daemon,
                target: Some("tcp://127.0.0.1:7777".to_string()),
            },
            crate::BuildVxExecutorClassConfig {
                class_label: "windows".to_string(),
                backend: vx_eval::ExecutorBackendKind::SshBootstrap,
                target: Some("amos@winbox".to_string()),
            },
        ]
    );
}

#[test]
fn parse_build_vx_executor_classes_rejects_duplicate_labels() {
    let error = crate::parse_build_vx_executor_classes("linux=local;linux=daemon:127.0.0.1:7777")
        .expect_err("duplicate labels should be rejected");
    assert!(error.to_string().contains("appears more than once"));
}

#[cfg(unix)]
#[test]
fn ssh_bootstrap_paths_use_a_stable_remote_layout() {
    assert_eq!(
        crate::ssh_bootstrap_paths("/home/amos", "linux"),
        crate::SshBootstrapPaths {
            bare_repo_dir: "/home/amos/.cache/vixen/ssh-bootstrap/linux/repo.git".to_string(),
            checkout_dir: "/home/amos/.cache/vixen/ssh-bootstrap/linux/repo".to_string(),
            vx_home: "/home/amos/.cache/vixen/ssh-bootstrap/linux/vx-home".to_string(),
            store_root: "/home/amos/.cache/vixen/ssh-bootstrap/linux/vx-home/store".to_string(),
            store_socket: "/home/amos/.cache/vixen/ssh-bootstrap/linux/vx-home/store.sock"
                .to_string(),
            vfsd_socket: "/home/amos/.cache/vixen/ssh-bootstrap/linux/vx-home/vfsd.sock"
                .to_string(),
            runner_socket: "/home/amos/.cache/vixen/ssh-bootstrap/linux/vx-home/runner.sock"
                .to_string(),
        }
    );
}

#[cfg(unix)]
#[test]
fn local_bootstrap_source_filter_skips_macos_generated_artifacts() {
    assert!(crate::should_skip_local_bootstrap_source_path(
        Utf8Path::new("apps/mac/xcode/build/DerivedData/foo")
    ));
    assert!(crate::should_skip_local_bootstrap_source_path(
        Utf8Path::new("apps/mac/dist/0.6.0/staging/Vixen.app")
    ));
    assert!(crate::should_skip_local_bootstrap_source_path(
        Utf8Path::new("zoo/libpng-src/.git/objects/pack/pack-foo")
    ));
    assert!(crate::should_skip_local_bootstrap_source_path(
        Utf8Path::new("some/nested/target/debug/foo")
    ));
    assert!(!crate::should_skip_local_bootstrap_source_path(
        Utf8Path::new("apps/mac/xcode/project.yml")
    ));
}

#[cfg(unix)]
#[test]
fn local_bootstrap_source_files_respect_gitignore() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let repo_root = Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf()).expect("utf8");
    std::fs::write(repo_root.join(".gitignore"), "ignored.txt\n").expect("write gitignore");
    std::fs::write(repo_root.join("kept.txt"), "kept").expect("write kept");
    std::fs::write(repo_root.join("ignored.txt"), "ignored").expect("write ignored");

    let (files, stats) =
        crate::local_bootstrap_source_files(&repo_root, &BTreeMap::new()).expect("collect files");

    assert_eq!(stats.hashed_count, 2);
    assert_eq!(stats.reused_count, 0);
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec![".gitignore", "kept.txt"]
    );
}

#[cfg(unix)]
#[test]
fn ssh_bootstrap_paths_slug_implicit_ssh_class_labels() {
    assert_eq!(
        crate::ssh_bootstrap_paths("/home/amos", "ssh:orb"),
        crate::SshBootstrapPaths {
            bare_repo_dir: "/home/amos/.cache/vixen/ssh-bootstrap/ssh-orb/repo.git".to_string(),
            checkout_dir: "/home/amos/.cache/vixen/ssh-bootstrap/ssh-orb/repo".to_string(),
            vx_home: "/home/amos/.cache/vixen/ssh-bootstrap/ssh-orb/vx-home".to_string(),
            store_root: "/home/amos/.cache/vixen/ssh-bootstrap/ssh-orb/vx-home/store".to_string(),
            store_socket: "/home/amos/.cache/vixen/ssh-bootstrap/ssh-orb/vx-home/store.sock"
                .to_string(),
            vfsd_socket: "/home/amos/.cache/vixen/ssh-bootstrap/ssh-orb/vx-home/vfsd.sock"
                .to_string(),
            runner_socket: "/home/amos/.cache/vixen/ssh-bootstrap/ssh-orb/vx-home/runner.sock"
                .to_string(),
        }
    );
}

#[cfg(unix)]
#[test]
fn ssh_bootstrap_windows_paths_use_tcp_layout() {
    let bootstrap_root = r"C:\Users\faste\.cache\vixen\ssh-bootstrap";
    let remote_home = r"C:\Users\faste";
    assert_eq!(
        crate::ssh_bootstrap_windows_paths(
            bootstrap_root,
            remote_home,
            "ssh:toast",
            57164,
            57165,
            57166
        ),
        crate::SshBootstrapWindowsPaths {
            bare_repo_dir: r"C:\Users\faste\.cache\vixen\ssh-bootstrap\ssh-toast\repo.git"
                .to_string(),
            checkout_dir: r"C:\Users\faste\.cache\vixen\ssh-bootstrap\ssh-toast\repo".to_string(),
            vx_home: r"C:\Users\faste\.cache\vixen\ssh-bootstrap\ssh-toast\vx-home".to_string(),
            store_root: r"C:\Users\faste\.cache\vixen\ssh-bootstrap\ssh-toast\vx-home\store"
                .to_string(),
            store_endpoint: "127.0.0.1:57164".to_string(),
            vfsd_endpoint: "127.0.0.1:57165".to_string(),
            runner_endpoint: "127.0.0.1:57166".to_string(),
        }
    );
}

#[cfg(unix)]
#[test]
fn windows_bootstrap_root_prefers_cargo_home_drive() {
    assert_eq!(
        crate::windows_bootstrap_root(
            r"C:\Users\faste",
            Some(r"X:\cargo"),
            Some(r"Y:\rustup"),
            Some(r"Z:\tmp"),
            Some(r"W:\temp"),
        ),
        r"X:\vixen\ssh-bootstrap"
    );
}

#[cfg(unix)]
#[test]
fn windows_bootstrap_root_falls_back_to_home_cache() {
    assert_eq!(
        crate::windows_bootstrap_root(r"C:\Users\faste", None, Some(r"rustup"), Some(r"tmp"), None,),
        r"C:\Users\faste\.cache\vixen\ssh-bootstrap"
    );
}

#[cfg(unix)]
#[test]
fn ssh_bootstrap_remote_start_script_daemonizes_runner() {
    assert!(crate::SSH_BOOTSTRAP_REMOTE_START_SCRIPT.contains("nohup"));
    assert!(crate::SSH_BOOTSTRAP_REMOTE_START_SCRIPT.contains("pkill -9 -f"));
    assert!(
        crate::SSH_BOOTSTRAP_REMOTE_START_SCRIPT
            .contains("\"$checkout_dir/target/release/vx-store\" --root \"$store_root\"")
    );
    assert!(
        crate::SSH_BOOTSTRAP_REMOTE_START_SCRIPT
            .contains("\"$checkout_dir/target/release/vx-runner\"")
    );
}

#[cfg(unix)]
#[test]
fn unix_bootstrap_target_triple_maps_supported_arches() {
    assert_eq!(
        crate::unix_bootstrap_target_triple("aarch64").expect("aarch64 target triple"),
        "aarch64-unknown-linux-gnu"
    );
    assert_eq!(
        crate::unix_bootstrap_target_triple("x86_64").expect("x86_64 target triple"),
        "x86_64-unknown-linux-gnu"
    );
}

#[cfg(unix)]
#[test]
fn windows_bootstrap_target_triple_maps_supported_arches() {
    assert_eq!(
        crate::windows_bootstrap_target_triple("x86_64").expect("x86_64 target triple"),
        "x86_64-pc-windows-msvc"
    );
    assert_eq!(
        crate::windows_bootstrap_target_triple("AMD64").expect("AMD64 target triple"),
        "x86_64-pc-windows-msvc"
    );
}

#[cfg(unix)]
#[test]
fn local_vx_bootstrap_stamp_path_uses_sidecar_file() {
    let path = Utf8Path::new("/tmp/vx-bootstrap.exe");
    assert_eq!(
        crate::local_vx_bootstrap_stamp_path(path),
        Utf8Path::new("/tmp/vx-bootstrap.exe.stamp")
    );
}

#[cfg(unix)]
#[test]
fn ssh_bootstrap_windows_command_announces_endpoints() {
    let bootstrap_root = r"C:\Users\faste\.cache\vixen\ssh-bootstrap";
    let remote_home = r"C:\Users\faste";
    let command = crate::ssh_bootstrap_windows_command(
        r"C:\Users\faste\.cache\vixen\ssh-bootstrap\ssh-toast\repo",
        &crate::ssh_bootstrap_windows_paths(
            bootstrap_root,
            remote_home,
            "ssh:toast",
            57164,
            57165,
            57166,
        ),
    );
    assert!(command.contains("-EncodedCommand "));
    let encoded = command
        .split_once("-EncodedCommand ")
        .map(|(_, encoded)| encoded)
        .expect("encoded PowerShell command");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("base64 decode");
    let utf16 = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let script = String::from_utf16(&utf16).expect("utf16 decode");
    assert!(script.contains("vx-bootstrap endpoints"));
    assert!(script.contains("$runnerEndpoint"));
    assert!(script.contains("target\\release\\vx-store.exe"));
    assert!(script.contains("target\\release\\vx-runner.exe"));
    assert!(script.contains("Start-Process -FilePath $runnerExe"));
}

#[cfg(unix)]
#[test]
fn ssh_bootstrap_windows_loader_command_reads_gzip_payload_and_execs_bootstrap() {
    let command = crate::ssh_bootstrap_windows_loader_command(
        r"X:\vixen\ssh-bootstrap\ssh-toast\vx-bootstrap.exe",
        r"X:\vixen\ssh-bootstrap\ssh-toast\vx-bootstrap.exe.gz",
    );
    assert!(command.contains("-EncodedCommand "));
    let encoded = command
        .split_once("-EncodedCommand ")
        .map(|(_, encoded)| encoded)
        .expect("encoded PowerShell command");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("base64 decode");
    let utf16 = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let script = String::from_utf16(&utf16).expect("utf16 decode");
    assert!(script.contains("$srcGz = 'X:\\vixen\\ssh-bootstrap\\ssh-toast\\vx-bootstrap.exe.gz'"));
    assert!(script.contains("Move-Item -Force $srcGz $tmpGz"));
    assert!(script.contains("vx-bootstrap-loader: decompressing bootstrap executable"));
    assert!(script.contains("System.IO.Compression.GzipStream"));
    assert!(script.contains("& $dest --stdio"));
}

#[cfg(unix)]
#[test]
fn ssh_control_master_not_ready_message_includes_log_detail() {
    let message = crate::ssh_control_master_not_ready_message(
        "toast",
        Some("Connection timed out during banner exchange"),
    );
    assert_eq!(
        message,
        "ssh control master for toast did not become ready: Connection timed out during banner exchange"
    );
}

static COLOR_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());
static TEST_SERVICE_BINARIES_READY: OnceLock<()> = OnceLock::new();

struct ColorOverrideGuard {
    _guard: MutexGuard<'static, ()>,
}

impl ColorOverrideGuard {
    fn always_ansi() -> Self {
        let guard = COLOR_OVERRIDE_LOCK.lock().expect("color override lock");
        concolor::set(ColorChoice::AlwaysAnsi);
        Self { _guard: guard }
    }
}

impl Drop for ColorOverrideGuard {
    fn drop(&mut self) {
        concolor::set(ColorChoice::Auto);
    }
}

fn ensure_test_service_binaries_built() {
    TEST_SERVICE_BINARIES_READY.get_or_init(|| {
        let _lock = lock_test_service_binary_build();
        let mut command = std::process::Command::new("cargo");
        command.args(["build", "-p", "vx-store"]);
        let status = command
            .status()
            .expect("failed to build vx-store test service binary");
        assert!(
            status.success(),
            "test service binary build failed with {status}"
        );
    });
}

fn lock_test_service_binary_build() -> TestFileLock {
    lock_test_file("service-binaries")
}

struct TestFileLock {
    file: std::fs::File,
}

impl Drop for TestFileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock_test_file(name: &str) -> TestFileLock {
    let lock_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/debug/.vxd-test-{name}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|error| panic!("failed to open {}: {error}", lock_path.display()));
    #[cfg(unix)]
    unsafe {
        if libc::flock(file.as_raw_fd(), libc::LOCK_EX) != 0 {
            let error = std::io::Error::last_os_error();
            panic!("failed to lock {}: {error}", lock_path.display());
        }
    }
    TestFileLock { file }
}

struct TestServiceBinaries {
    _dir: TempDir,
    store: String,
}

fn copy_test_service_binaries() -> TestServiceBinaries {
    ensure_test_service_binaries_built();
    let dir = TempDir::new().expect("failed to create test service binary tempdir");
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("create test service binary dir");

    let store = copy_test_service_binary(&resolve_vx_store_binary(), &bin_dir, "vx-store");

    TestServiceBinaries { _dir: dir, store }
}

fn copy_test_service_binary(source: &str, bin_dir: &std::path::Path, name: &str) -> String {
    let destination = bin_dir.join(name);
    fs::copy(source, &destination).unwrap_or_else(|error| {
        panic!(
            "failed to copy {source} to {}: {error}",
            destination.display()
        )
    });
    let permissions = fs::metadata(source)
        .unwrap_or_else(|error| panic!("failed to stat {source}: {error}"))
        .permissions();
    fs::set_permissions(&destination, permissions).unwrap_or_else(|error| {
        panic!(
            "failed to set permissions on {}: {error}",
            destination.display()
        )
    });
    destination.to_string_lossy().into_owned()
}

fn vixen_grammar_store() -> Result<Arc<GrammarStore>, Box<dyn std::error::Error>> {
    let grammar = Arc::new(CompiledGrammar::new(GrammarConfig {
        language: tree_sitter_vixen::language().into(),
        highlights_query: tree_sitter_vixen::HIGHLIGHTS_QUERY,
        injections_query: tree_sitter_vixen::INJECTIONS_QUERY,
        locals_query: tree_sitter_vixen::LOCALS_QUERY,
    })?);

    let store = Arc::new(GrammarStore::new());
    store.insert("vixen", grammar);
    Ok(store)
}

fn line_order(source: &str, offset: usize) -> i32 {
    Source::from(source)
        .get_offset_line(offset)
        .map(|(_, line, _)| line as i32)
        .expect("offset should map to a source line")
}

#[test]
fn test_sanitize_id_component_replaces_non_identifier_chars() {
    assert_eq!(
        crate::service::sanitize_id_component("hello/world::crate@1.2.3"),
        "hello_world__crate_1_2_3"
    );
}

#[test]
fn build_vx_exec_summary_includes_args_mounts_and_writable_roots() {
    let empty_module = vx_eval::TransportModule {
        source_id: "/workspace/build.vx".to_string(),
        functions: Vec::new(),
        top_level: TransportBlock { stmts: Vec::new() },
        loaded_modules: Vec::new(),
    };
    let empty_function = ShippedFunction {
        module_id: "/workspace/build.vx".to_string(),
        code_identity: "observer".to_string(),
        def: TransportFnDef {
            name: "observer".to_string(),
            params: Vec::new(),
            keyword_params: Vec::new(),
            body: TransportBlock { stmts: Vec::new() },
            provenance: None,
        },
        captures: Vec::new(),
    };
    let summary = crate::service::summarize_build_vx_exec_request(&ExecRequest {
        args: vec![
            ExecArgValue::Literal("/usr/bin/cc".to_string()),
            ExecArgValue::Literal("-x".to_string()),
            ExecArgValue::Literal("c".to_string()),
            ExecArgValue::Path(PathValue::new("probe.c")),
            ExecArgValue::Literal("-c".to_string()),
            ExecArgValue::Literal("-o".to_string()),
            ExecArgValue::Path(PathValue::new("out/probe.o")),
        ],
        env: Vec::new(),
        mounts: vec![vx_eval::ExecMount {
            path: PathValue::new("probe.c"),
            value: Value::Blob(BlobValue::from_string("int main(void) { return 0; }\n")),
            mode: vx_eval::ExecMountMode::Tracked,
        }],
        writable_roots: vec![PathValue::new("out")],
        create_dirs: vec![PathValue::new("out/obj")],
        executor: vx_eval::ExecutorSelector::class("default"),
        demand_chain: DemandChain::new(),
        module: empty_module,
        observer: empty_function,
        verification_obligations: Vec::new(),
    });

    assert_eq!(
        summary,
        "build.vx exec[class:default]: /usr/bin/cc, -x, c, probe.c, -c, -o, out/probe.o (mounts: probe.c; writable: out; create_dirs: out/obj)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn store_exec_mounts_accepts_discovered_blob_leaves() {
    let env = StoreTestEnv::new().await;
    let blob = BlobValue::Inline {
        bytes: Arc::from("echo".as_bytes()),
        executable: true,
    };
    let mounts = vec![vx_eval::ExecMount {
        path: PathValue::new("bin"),
        value: Value::Tree(TreeValue::inline(Arc::from([(
            Arc::from("tool"),
            Value::Discovered(vx_eval::DiscoveredValue {
                value: Box::new(Value::Blob(blob)),
                obligations: Arc::from([]),
            }),
        )]))),
        mode: vx_eval::ExecMountMode::Opaque,
    }];

    let root = crate::service::store_exec_mounts(&env.client, &mounts)
        .await
        .expect("discovered executable blob leaf should store successfully")
        .expect("mount tree root");
    let materialized = TempDir::new().expect("materialize root");
    let output_root = Utf8Path::from_path(materialized.path()).expect("utf8 tempdir");

    crate::service::materialize_store_tree_into_dir(&env.client, root, output_root)
        .expect("materialize stored mount tree");

    assert!(materialized.path().join("bin/tool").exists());
}

#[test]
fn exec_mounts_singleflight_key_distinguishes_mount_identity() {
    let host_mounts = vec![
        vx_eval::ExecMount {
            path: PathValue::new("src"),
            value: Value::Tree(TreeValue::FsDir(PathValue::new("/tmp/libpng-src"))),
            mode: vx_eval::ExecMountMode::Tracked,
        },
        vx_eval::ExecMount {
            path: PathValue::new("gen"),
            value: Value::Tree(TreeValue::inline(Arc::from([(
                Arc::from("pnglibconf.h"),
                Value::ExecPath(ExecPathValue::InTree {
                    tree: TreeValue::FsDir(PathValue::new("/tmp/libpng-src")),
                    suffix: PathValue::new("scripts/pnglibconf.h.prebuilt"),
                }),
            )]))),
            mode: vx_eval::ExecMountMode::Tracked,
        },
    ];
    let same_mounts = host_mounts.clone();
    let reordered_mounts = vec![host_mounts[1].clone(), host_mounts[0].clone()];
    let different_mounts = vec![
        vx_eval::ExecMount {
            path: PathValue::new("src"),
            value: Value::Tree(TreeValue::FsDir(PathValue::new("/tmp/libpng-src-other"))),
            mode: vx_eval::ExecMountMode::Tracked,
        },
        vx_eval::ExecMount {
            path: PathValue::new("gen"),
            value: Value::Tree(TreeValue::inline(Arc::from([(
                Arc::from("pnglibconf.h"),
                Value::ExecPath(ExecPathValue::InTree {
                    tree: TreeValue::FsDir(PathValue::new("/tmp/libpng-src-other")),
                    suffix: PathValue::new("scripts/pnglibconf.h.prebuilt"),
                }),
            )]))),
            mode: vx_eval::ExecMountMode::Tracked,
        },
    ];

    let first = crate::service::exec_mounts_singleflight_key(&host_mounts).expect("key");
    let second = crate::service::exec_mounts_singleflight_key(&same_mounts).expect("key");
    let reordered = crate::service::exec_mounts_singleflight_key(&reordered_mounts).expect("key");
    let different = crate::service::exec_mounts_singleflight_key(&different_mounts).expect("key");

    assert_eq!(first, second);
    assert_eq!(first, reordered);
    assert_ne!(first, different);
}

#[test]
fn build_vx_discover_summary_includes_executor_class() {
    let empty_module = vx_eval::TransportModule {
        source_id: "/workspace/build.vx".to_string(),
        functions: Vec::new(),
        top_level: TransportBlock { stmts: Vec::new() },
        loaded_modules: Vec::new(),
    };
    let probe = ShippedFunction {
        module_id: "/workspace/build.vx".to_string(),
        code_identity: "probe".to_string(),
        def: TransportFnDef {
            name: "probe".to_string(),
            params: Vec::new(),
            keyword_params: Vec::new(),
            body: TransportBlock { stmts: Vec::new() },
            provenance: None,
        },
        captures: Vec::new(),
    };
    let summary = crate::service::summarize_build_vx_discover_request(&EvalDiscoverRequest {
        executor: vx_eval::ExecutorSelector::class("default"),
        demand_chain: DemandChain::new(),
        module: empty_module,
        probe,
    });

    assert_eq!(summary, "build.vx discover[class:default]");
}

#[test]
fn build_vx_virtual_config_import_uses_config_path() {
    let config_path = Utf8PathBuf::from("/tmp/vixen-config.vx");
    let resolved = crate::service::resolve_build_vx_import_path(
        Some(&config_path),
        &EvalImportRequest {
            path: crate::service::BUILD_VX_CONFIG_IMPORT_PATH.to_string(),
            from_source_id: "/workspace/build.vx".to_string(),
            demand_chain: DemandChain::new(),
        },
    )
    .expect("virtual config import should resolve");

    assert_eq!(resolved, config_path);
}

#[test]
fn build_vx_virtual_config_import_requires_config_path() {
    let error = crate::service::resolve_build_vx_import_path(
        None,
        &EvalImportRequest {
            path: crate::service::BUILD_VX_CONFIG_IMPORT_PATH.to_string(),
            from_source_id: "/workspace/build.vx".to_string(),
            demand_chain: DemandChain::new(),
        },
    )
    .expect_err("missing config path should be rejected");

    assert!(
        error
            .to_string()
            .contains("VIXEN_CONFIG_PATH is not set and no default config was found")
    );
}

#[test]
fn test_legacy_compile_node_id_format() {
    assert_eq!(
        crate::service::legacy_compile_node_id("my-crate", "bin"),
        "compile-bin:my-crate"
    );
}

#[test]
fn test_rule_plan_parity_detects_missing_focus_nodes() {
    let legacy = GraphTopology {
        nodes: vec![GraphNode {
            index: 0,
            node_id: "compile-bin:app".to_string(),
            kind: "rust.compile_bin".to_string(),
            label: "compile app".to_string(),
        }],
        edges: vec![],
    };
    let rule_plan = GraphTopology {
        nodes: vec![],
        edges: vec![],
    };

    let diff = crate::service::compare_rule_plan_parity(&legacy, &rule_plan);
    assert_eq!(diff.legacy_focus_count, 1);
    assert_eq!(diff.rule_plan_focus_count, 0);
    assert_eq!(
        diff.missing_in_rule_plan,
        vec!["compile-bin:app".to_string()]
    );
    assert!(diff.extra_in_rule_plan.is_empty());
}

#[test]
fn test_rule_plan_parity_ignores_non_focus_nodes() {
    let legacy = GraphTopology {
        nodes: vec![GraphNode {
            index: 0,
            node_id: "download toolchain Rustc".to_string(),
            kind: "toolchain.component.download".to_string(),
            label: "download toolchain Rustc".to_string(),
        }],
        edges: vec![GraphEdge {
            from: 0,
            to: 0,
            kind: Some("always".to_string()),
        }],
    };
    let rule_plan = GraphTopology {
        nodes: vec![GraphNode {
            index: 0,
            node_id: "planner-action:n_action".to_string(),
            kind: "planner.action".to_string(),
            label: "planner action".to_string(),
        }],
        edges: vec![],
    };

    let diff = crate::service::compare_rule_plan_parity(&legacy, &rule_plan);
    assert_eq!(diff.legacy_focus_count, 0);
    assert_eq!(diff.rule_plan_focus_count, 0);
    assert!(diff.missing_in_rule_plan.is_empty());
    assert!(diff.extra_in_rule_plan.is_empty());
}

#[test]
fn arborium_can_highlight_vixen_via_external_grammar_crate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut highlighter = Highlighter::with_store(vixen_grammar_store()?);
    let html = highlighter.highlight(
        "vixen",
        r#"fn build(ws: Path) -> Tree (
  let src_tree = Tree(ws / @zoo/lua-src);
  src_tree
)"#,
    )?;
    println!("{html}");

    assert!(html.contains("<a-"), "{html}");
    assert!(html.contains("build"), "{html}");
    assert!(html.contains("lua-src"), "{html}");

    Ok(())
}

#[test]
fn ariadne_can_render_vixen_source_with_arborium_color_labels()
-> Result<(), Box<dyn std::error::Error>> {
    let _color = ColorOverrideGuard::always_ansi();
    let source = r#"fn build(ws: Path) -> Tree (
  let src_tree = Tree(ws / @zoo/lua-src);
  src_tree
)"#;
    let filename = "build.vx";
    let semantic_text = "@zoo/lua-src";
    let semantic_start = source.find(semantic_text).expect("semantic label start");
    let semantic_range = semantic_start..semantic_start + semantic_text.len();

    let mut highlighter = Highlighter::with_store(vixen_grammar_store()?);
    let raw_spans = highlighter.highlight_spans("vixen", source)?;
    let themed_spans = spans_to_themed(raw_spans);
    let theme = builtin::catppuccin_mocha();
    let mut syntax_label_count = 0usize;

    let mut report = Report::build(ReportKind::Error, (filename, semantic_range.clone()))
        .with_config(AriadneConfig::default().with_color(true))
        .with_message("build input not found");

    for span in themed_spans {
        if span.start >= span.end {
            continue;
        }

        let Some(style) = theme.style(span.theme_index) else {
            continue;
        };
        let Some(fg) = style.fg else {
            continue;
        };

        syntax_label_count += 1;
        report = report.with_label(
            Label::new((filename, span.start as usize..span.end as usize))
                .with_order(line_order(source, span.start as usize))
                .with_priority(0)
                .with_color(AriadneColor::Rgb(fg.r, fg.g, fg.b)),
        );
    }

    assert!(syntax_label_count > 4, "expected syntax-colored labels");

    let mut out = Vec::new();
    report
        .with_label(
            Label::new((filename, semantic_range))
                .with_order(line_order(source, semantic_start))
                .with_priority(10)
                .with_color(AriadneColor::Red)
                .with_message("this path does not exist in the current workspace"),
        )
        .with_note("resolved path: /workspace/zoo/lua-src")
        .with_note("workspace root: /workspace")
        .with_note("os error: No such file or directory")
        .finish()
        .write((filename, Source::from(source)), &mut out)?;

    assert!(
        out.contains(&b'\x1b'),
        "expected ANSI escape codes in forced-color output: {out:?}"
    );

    let rendered = String::from_utf8(out)?;
    println!("{rendered}");
    assert!(rendered.contains("build input not found"), "{rendered}");
    assert!(
        rendered.contains("this path does not exist in the current workspace"),
        "{rendered}"
    );
    assert!(rendered.contains("lua-src"), "{rendered}");
    assert!(
        rendered.contains("resolved path: /workspace/zoo/lua-src"),
        "{rendered}"
    );
    assert_eq!(rendered.matches("build.vx:").count(), 1, "{rendered}");

    Ok(())
}

#[test]
fn test_rule_plan_edge_parity_compares_focus_deps_with_two_hop_bridge() {
    // Legacy: compile(app) depends directly on compile(dep) and registry acquire.
    let legacy = GraphTopology {
        nodes: vec![
            GraphNode {
                index: 0,
                node_id: "compile-bin:app".to_string(),
                kind: "rust.compile_bin".to_string(),
                label: "compile app".to_string(),
            },
            GraphNode {
                index: 1,
                node_id: "compile-lib:dep".to_string(),
                kind: "rust.compile_rlib".to_string(),
                label: "compile dep".to_string(),
            },
            GraphNode {
                index: 2,
                node_id: "acquire-registry:serde:1.0.0".to_string(),
                kind: "registry.acquire".to_string(),
                label: "acquire serde".to_string(),
            },
        ],
        edges: vec![
            GraphEdge {
                from: 0,
                to: 1,
                kind: Some("always".to_string()),
            },
            GraphEdge {
                from: 0,
                to: 2,
                kind: Some("always".to_string()),
            },
        ],
    };

    // RulePlan-lowered: compile(app) depends on dep_artifact (non-focus) and registry acquire.
    // dep_artifact depends on compile(dep). This should still count as a focus dep via 2-hop walk.
    let rule_plan = GraphTopology {
        nodes: vec![
            GraphNode {
                index: 0,
                node_id: "compile-bin:app".to_string(),
                kind: "rust.compile_bin".to_string(),
                label: "compile app".to_string(),
            },
            GraphNode {
                index: 1,
                node_id: "artifact:dep".to_string(),
                kind: "planner.artifact".to_string(),
                label: "artifact dep".to_string(),
            },
            GraphNode {
                index: 2,
                node_id: "compile-lib:dep".to_string(),
                kind: "rust.compile_rlib".to_string(),
                label: "compile dep".to_string(),
            },
            GraphNode {
                index: 3,
                node_id: "acquire-registry:serde:1.0.0".to_string(),
                kind: "registry.acquire".to_string(),
                label: "acquire serde".to_string(),
            },
        ],
        edges: vec![
            GraphEdge {
                from: 0,
                to: 1,
                kind: Some("ifchanged".to_string()),
            },
            GraphEdge {
                from: 1,
                to: 2,
                kind: Some("ifwritten".to_string()),
            },
            GraphEdge {
                from: 0,
                to: 3,
                kind: Some("ifchanged".to_string()),
            },
        ],
    };

    let diff = crate::service::compare_rule_plan_edge_parity(&legacy, &rule_plan);
    assert!(diff.missing_in_rule_plan.is_empty(), "{diff:?}");
    assert!(diff.extra_in_rule_plan.is_empty(), "{diff:?}");
}

#[test]
fn test_rule_plan_execution_selects_crate_subset_and_closure() {
    use std::collections::{BTreeMap, HashMap, HashSet};
    use vx_manifest::Edition;
    use vx_report::{RuleId, RulePlan, RulePlanNode, RulePlanNodeId, RulePlanNodeKind};
    use vx_rs::crate_graph::{CrateGraph, CrateId, CrateNode, CrateSource, CrateType, DepEdge};

    let workspace_root = camino::Utf8PathBuf::from("/workspace");

    let dep_source = CrateSource::Path {
        workspace_rel: camino::Utf8PathBuf::from("dep"),
    };
    let dep_id = CrateId::from_source(&dep_source, "dep");
    let dep_node = CrateNode {
        id: dep_id,
        package_name: "dep".to_string(),
        crate_name: "dep".to_string(),
        source: dep_source,
        workspace_rel: camino::Utf8PathBuf::from("dep"),
        crate_root_rel: camino::Utf8PathBuf::from("dep/src/lib.rs"),
        edition: Edition::E2021,
        crate_type: CrateType::Lib,
        deps: vec![],
        build_script_rel: None,
        build_script_output: None,
    };

    let app_source = CrateSource::Path {
        workspace_rel: camino::Utf8PathBuf::from("app"),
    };
    let app_id = CrateId::from_source(&app_source, "app");
    let app_node = CrateNode {
        id: app_id,
        package_name: "app".to_string(),
        crate_name: "app".to_string(),
        source: app_source,
        workspace_rel: camino::Utf8PathBuf::from("app"),
        crate_root_rel: camino::Utf8PathBuf::from("app/src/main.rs"),
        edition: Edition::E2021,
        crate_type: CrateType::Bin,
        deps: vec![DepEdge {
            extern_name: "dep".to_string(),
            crate_id: dep_id,
        }],
        build_script_rel: None,
        build_script_output: None,
    };

    let graph = CrateGraph {
        workspace_root: workspace_root.clone(),
        nodes: HashMap::from([(dep_id, dep_node), (app_id, app_node)]),
        topo_order: vec![dep_id, app_id],
        root_crate: app_id,
        registry_crates: vec![],
        reachable_packages: None,
    };

    let provenance = vx_report::RulePlanProvenance {
        rule_id: RuleId::from("rule.test".to_string()),
        source_id: vx_report::RulePlanSourceId::from("src".to_string()),
        span: vx_report::RulePlanSpan {
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
        },
        comment: None,
        metadata: {
            let mut m = BTreeMap::new();
            m.insert("crate_name".to_string(), "app".to_string());
            m.insert("crate_type".to_string(), "bin".to_string());
            m
        },
    };

    let plan = RulePlan {
        sources: vec![],
        nodes: vec![RulePlanNode {
            id: RulePlanNodeId::from("n_action_app".to_string()),
            kind: RulePlanNodeKind::Action,
            label: "compile-bin:app".to_string(),
            provenance,
        }],
        edges: vec![],
        facts: vec![],
    };

    let selected = crate::service::selected_crates_from_rule_plan(&plan, &graph).unwrap();
    assert_eq!(selected.len(), 1);
    assert!(selected.contains(&app_id));

    let closure = crate::service::transitive_closure_crates(&graph, &selected);
    assert_eq!(closure, HashSet::from([app_id, dep_id]));
}

/// Set up common build configuration for C builds
fn setup_cc_config(db: &Database) -> BuildConfig {
    let build_key = BuildConfig::compute_key("debug", "x86_64-linux-musl", "/test/workspace", "");
    let config = BuildConfig::new(
        db,
        build_key,
        "debug".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
        String::new(),
    )
    .unwrap();

    ZigToolchainConfig::set(db, "0.13.0".to_string(), "zig:abc123def456".to_string()).unwrap();

    config
}

#[vx_test]
async fn test_cache_key_cc_compile_deterministic() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let source_hash = Blake3Hash::from_bytes(b"int main() { return 0; }");

    // Create source file input
    let source = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();

    // Compute cache key twice
    let key1 = cache_key_cc_compile(&db, source, config).await.unwrap();
    let key2 = cache_key_cc_compile(&db, source, config).await.unwrap();

    // Should be identical (deterministic)
    assert_eq!(key1, key2);
}

#[vx_test]
async fn test_cache_key_cc_compile_changes_with_source() {
    let db = test_db();
    let config = setup_cc_config(&db);

    // First source
    let source1 = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 0; }"),
    )
    .unwrap();

    let key1 = cache_key_cc_compile(&db, source1, config).await.unwrap();

    // Different source content
    let source2 = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 1; }"),
    )
    .unwrap();

    let key2 = cache_key_cc_compile(&db, source2, config).await.unwrap();

    // Keys should be different
    assert_ne!(key1, key2);
}

#[vx_test]
async fn test_cache_key_cc_compile_changes_with_profile() {
    let db = test_db();

    ZigToolchainConfig::set(&db, "0.13.0".to_string(), "zig:abc123def456".to_string()).unwrap();

    let source_hash = Blake3Hash::from_bytes(b"int main() { return 0; }");

    // Debug profile
    let build_key_debug =
        BuildConfig::compute_key("debug", "x86_64-linux-musl", "/test/workspace", "");
    let source_debug = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let config_debug = BuildConfig::new(
        &db,
        build_key_debug,
        "debug".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
        String::new(),
    )
    .unwrap();
    let key_debug = cache_key_cc_compile(&db, source_debug, config_debug)
        .await
        .unwrap();

    // Release profile
    let build_key_release =
        BuildConfig::compute_key("release", "x86_64-linux-musl", "/test/workspace", "");
    let config_release = BuildConfig::new(
        &db,
        build_key_release,
        "release".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
        String::new(),
    )
    .unwrap();

    let source_release = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let key_release = cache_key_cc_compile(&db, source_release, config_release)
        .await
        .unwrap();

    // Keys should be different
    assert_ne!(key_debug, key_release);
}

#[vx_test]
async fn test_node_id_cc_compile_format() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let source = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 0; }"),
    )
    .unwrap();

    let node_id = node_id_cc_compile(&db, source, config).await.unwrap();

    // Should be formatted as "cc-compile:path:triple:profile"
    assert!(node_id.0.starts_with("cc-compile:"));
    assert!(node_id.0.contains("src/main.c"));
    assert!(node_id.0.contains("x86_64-linux-musl"));
    assert!(node_id.0.contains("debug"));
}

#[vx_test]
async fn test_cache_key_cc_link_deterministic() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let obj_hashes = vec![
        Blake3Hash::from_bytes(b"main.o contents"),
        Blake3Hash::from_bytes(b"util.o contents"),
    ];

    let target = CTarget::new(
        &db,
        "hello".to_string(),
        obj_hashes.clone(),
        vec!["src/main.c".to_string(), "src/util.c".to_string()],
    )
    .unwrap();

    let key1 = cache_key_cc_link(&db, target, config).await.unwrap();
    let key2 = cache_key_cc_link(&db, target, config).await.unwrap();

    assert_eq!(key1, key2);
}

#[vx_test]
async fn test_cache_key_cc_link_changes_with_objects() {
    let db = test_db();
    let config = setup_cc_config(&db);

    // First set of objects
    let target1 = CTarget::new(
        &db,
        "hello".to_string(),
        vec![Blake3Hash::from_bytes(b"main.o v1")],
        vec!["src/main.c".to_string()],
    )
    .unwrap();

    let key1 = cache_key_cc_link(&db, target1, config).await.unwrap();

    // Different object contents
    let target2 = CTarget::new(
        &db,
        "hello".to_string(),
        vec![Blake3Hash::from_bytes(b"main.o v2")],
        vec!["src/main.c".to_string()],
    )
    .unwrap();

    let key2 = cache_key_cc_link(&db, target2, config).await.unwrap();

    assert_ne!(key1, key2);
}

#[vx_test]
async fn test_node_id_cc_link_format() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let target = CTarget::new(
        &db,
        "hello".to_string(),
        vec![Blake3Hash::from_bytes(b"main.o")],
        vec!["src/main.c".to_string()],
    )
    .unwrap();

    let node_id = node_id_cc_link(&db, target, config).await.unwrap();

    // Should be formatted as "cc-link:name:triple:profile"
    assert!(node_id.0.starts_with("cc-link:"));
    assert!(node_id.0.contains("hello"));
    assert!(node_id.0.contains("x86_64-linux-musl"));
    assert!(node_id.0.contains("debug"));
}

// =============================================================================
// GOLDEN-FILE TESTS: Cache Key Stability
// =============================================================================
//
// These tests pin exact cache key values for known inputs. If any test fails,
// it means cache key computation changed — which silently invalidates every
// user's cache. That's either:
//   (a) intentional: update the golden value and note what changed
//   (b) accidental: revert whatever broke it
//
// See: https://github.com/bearcove/vixen/issues/131

/// Fixed toolchain ID used across all golden tests (blake3 of "golden-toolchain-id")
fn golden_toolchain_id() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-toolchain-id")
}

/// Fixed toolchain manifest hash used across all golden tests
fn golden_toolchain_manifest() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-toolchain-manifest")
}

/// Fixed source closure hash used across all golden tests
fn golden_closure_hash() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-source-closure")
}

/// Fixed cargo.toml content hash used across all golden tests
fn golden_cargo_hash() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-cargo-toml")
}

fn setup_golden_rust(db: &Database) -> (RustToolchain, BuildConfig) {
    let toolchain = RustToolchain::new(
        db,
        golden_toolchain_id(),
        golden_toolchain_manifest(),
        "x86_64-unknown-linux-gnu".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
    )
    .unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "",
    );
    let config = BuildConfig::new(
        db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        String::new(),
    )
    .unwrap();

    (toolchain, config)
}

#[vx_test]
async fn golden_cache_key_compile_bin() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    CargoToml::set(
        &db,
        golden_cargo_hash(),
        "hello-world".to_string(),
        vx_manifest::Edition::E2021,
        "src/main.rs".to_string(),
    )
    .unwrap();
    SourceClosure::set(&db, golden_closure_hash(), vec!["src/main.rs".to_string()]).unwrap();

    let key = cache_key_compile_bin(&db, toolchain, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "5643b5f60dd42579ac9e556ac248a210cfd0ad3495e55c1e3b302c35feacb55e",
        "cache_key_compile_bin golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_rlib() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    let crate_info = RustCrate::new(
        &db,
        "crate-abc123".to_string(),
        "my_lib".to_string(),
        "2021".to_string(),
        "lib".to_string(),
        "src/lib.rs".to_string(),
        golden_closure_hash(),
    )
    .unwrap();

    let key = cache_key_compile_rlib(&db, crate_info, toolchain, config)
        .await
        .unwrap();
    assert_eq!(
        key.to_hex(),
        "7bb67e2007282c850c38320be1be8a40a7158d2dda2c7d6f3de84d887359a8bb",
        "cache_key_compile_rlib golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_rlib_with_deps() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    let crate_info = RustCrate::new(
        &db,
        "crate-abc123".to_string(),
        "my_lib".to_string(),
        "2021".to_string(),
        "lib".to_string(),
        "src/lib.rs".to_string(),
        golden_closure_hash(),
    )
    .unwrap();

    let dep_hashes = vec![
        ("log".to_string(), Blake3Hash::from_bytes(b"log-rlib-hash")),
        (
            "serde".to_string(),
            Blake3Hash::from_bytes(b"serde-rlib-hash"),
        ),
    ];

    let key = cache_key_compile_rlib_with_deps(&db, crate_info, toolchain, config, dep_hashes)
        .await
        .unwrap();
    assert_eq!(
        key.to_hex(),
        "b2c43bb8ae50404c7d4d62cca40fd1bc6084767025bb17c3847bca7db826a721",
        "cache_key_compile_rlib_with_deps golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_bin_with_deps() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    let crate_info = RustCrate::new(
        &db,
        "crate-abc123".to_string(),
        "my_app".to_string(),
        "2021".to_string(),
        "bin".to_string(),
        "src/main.rs".to_string(),
        golden_closure_hash(),
    )
    .unwrap();

    let dep_hashes = vec![
        ("log".to_string(), Blake3Hash::from_bytes(b"log-rlib-hash")),
        (
            "serde".to_string(),
            Blake3Hash::from_bytes(b"serde-rlib-hash"),
        ),
    ];

    let key = cache_key_compile_bin_with_deps(&db, crate_info, toolchain, config, dep_hashes)
        .await
        .unwrap();
    assert_eq!(
        key.to_hex(),
        "51c6377ddc4cbedfa5f01a82ed251ba0640f86c3d36e89e32f0206e7f7e6bc63",
        "cache_key_compile_bin_with_deps golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_cc_compile() {
    let db = test_db();

    ZigToolchainConfig::set(&db, "0.13.0".to_string(), "zig-golden-id".to_string()).unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "",
    );
    let config = BuildConfig::new(
        &db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        String::new(),
    )
    .unwrap();

    let source = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 0; }"),
    )
    .unwrap();

    let key = cache_key_cc_compile(&db, source, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "aa13b9c630011d5478b31ac1908d9e65fd528d9cbac3eb5a8fc573f3045caa85",
        "cache_key_cc_compile golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_cc_link() {
    let db = test_db();

    ZigToolchainConfig::set(&db, "0.13.0".to_string(), "zig-golden-id".to_string()).unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "",
    );
    let config = BuildConfig::new(
        &db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        String::new(),
    )
    .unwrap();

    let target = CTarget::new(
        &db,
        "hello".to_string(),
        vec![
            Blake3Hash::from_bytes(b"main.o golden"),
            Blake3Hash::from_bytes(b"util.o golden"),
        ],
        vec!["src/main.c".to_string(), "src/util.c".to_string()],
    )
    .unwrap();

    let key = cache_key_cc_link(&db, target, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "293a18ee13eb527b0de56213a002cf5cb22bc6748c99ef059362cce0411f59fb",
        "cache_key_cc_link golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_bin_with_session() {
    let db = test_db();

    let toolchain = RustToolchain::new(
        &db,
        golden_toolchain_id(),
        golden_toolchain_manifest(),
        "x86_64-unknown-linux-gnu".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
    )
    .unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "test-session-42",
    );
    let config = BuildConfig::new(
        &db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        "test-session-42".to_string(),
    )
    .unwrap();

    CargoToml::set(
        &db,
        golden_cargo_hash(),
        "hello-world".to_string(),
        vx_manifest::Edition::E2021,
        "src/main.rs".to_string(),
    )
    .unwrap();
    SourceClosure::set(&db, golden_closure_hash(), vec!["src/main.rs".to_string()]).unwrap();

    let key = cache_key_compile_bin(&db, toolchain, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "0c0fd85eb4f870b378e56e6cb02363837aff4aff80f2146d9ec0b227fc8da5c0",
        "cache_key_compile_bin (with session) golden value changed! \
         If intentional, update this value."
    );
}

// =============================================================================
// EXISTING TESTS
// =============================================================================

#[vx_test]
async fn test_discovered_deps_affects_cache_key() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let source_hash = Blake3Hash::from_bytes(b"#include \"header.h\"\nint main() {}");

    // First compile: no discovered deps yet
    let source1 = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let key1 = cache_key_cc_compile(&db, source1, config).await.unwrap();

    // After first compile, we discover header.h dependency
    // Set discovered deps for this translation unit
    let tu_key = "cc:src/main.c:debug:x86_64-linux-musl".to_string();
    let deps_hash = Blake3Hash::from_bytes(b"include/header.h");
    DiscoveredDeps::new(&db, tu_key, deps_hash, vec!["include/header.h".to_string()]).unwrap();

    // Second compile with same source but now with discovered deps
    let source2 = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let key2 = cache_key_cc_compile(&db, source2, config).await.unwrap();

    // Cache keys should be different because discovered deps changed
    assert_ne!(key1, key2);
}

#[vx_test]
async fn test_store_backed_build_vx_trees_materialize_from_cas() {
    let env = StoreTestEnv::new().await;

    let bytes = b"hello from store";
    let prepared = prepare_blob(bytes);
    let blob_ref = store_prepared_blob(&env.client, &prepared, bytes)
        .await
        .expect("blob should store");
    let file_kind = dir_entry_kind_from_blob_ref(&blob_ref, false);
    let (root, _total_size) = crate::cas_tree::build_merkle_tree(
        &env.client,
        vec![crate::cas_tree::TreeFile {
            path: "nested/greeting.txt".to_string(),
            kind: file_kind,
        }],
    )
    .await
    .expect("tree should store");

    let output = TempDir::new().expect("failed to create output tempdir");
    let output_root = Utf8Path::from_path(output.path()).expect("tempdir should be utf-8");
    crate::service::materialize_store_tree_into_dir_async(&env.client, root, output_root)
        .await
        .expect("store-backed tree should materialize");

    let rendered = fs::read_to_string(output.path().join("nested/greeting.txt"))
        .expect("materialized file should be readable");
    assert_eq!(rendered, "hello from store");
}

#[vx_test]
#[cfg(unix)]
async fn test_store_backed_build_vx_trees_preserve_executable_bits() {
    use std::os::unix::fs::PermissionsExt;

    let env = StoreTestEnv::new().await;

    let bytes = b"#!/bin/sh\nexit 0\n";
    let prepared = prepare_blob(bytes);
    let blob_ref = store_prepared_blob(&env.client, &prepared, bytes)
        .await
        .expect("blob should store");
    let file_kind = dir_entry_kind_from_blob_ref(&blob_ref, true);
    let (root, _total_size) = crate::cas_tree::build_merkle_tree(
        &env.client,
        vec![crate::cas_tree::TreeFile {
            path: "bin/tool".to_string(),
            kind: file_kind,
        }],
    )
    .await
    .expect("tree should store");

    let output = TempDir::new().expect("failed to create output tempdir");
    let output_root = Utf8Path::from_path(output.path()).expect("tempdir should be utf-8");
    crate::service::materialize_store_tree_into_dir_async(&env.client, root, output_root)
        .await
        .expect("store-backed tree should materialize");

    let executable = output.path().join("bin/tool");
    let mode = fs::metadata(&executable)
        .expect("materialized file should stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755, "expected executable mode on materialized file");
}

struct StoreTestEnv {
    _root: TempDir,
    _vx_home: TempDir,
    _binaries: TestServiceBinaries,
    _service: ServiceHandle,
    client: Arc<StoreClient>,
}

impl StoreTestEnv {
    async fn new() -> Self {
        let binaries = copy_test_service_binaries();
        let root = TempDir::new().expect("failed to create store root tempdir");
        let vx_home = TempDir::new().expect("failed to create vx_home tempdir");
        let endpoint = free_local_endpoint();
        let root_str = root.path().to_string_lossy().into_owned();

        let service = ServiceBuilder::new(&binaries.store, endpoint)
            .caller_name("vxd-store-materialize-tests")
            .args(&["--root", &root_str])
            .env("VX_TEST", "1")
            .timeout(std::time::Duration::from_secs(10))
            .connect()
            .await
            .expect("failed to spawn/connect vx-store");

        let virtual_handle = service
            .boot_and_connect()
            .await
            .expect("failed to boot/connect vx-store virtual connection");

        Self {
            _root: root,
            _vx_home: vx_home,
            _binaries: binaries,
            _service: service,
            client: Arc::new(StoreClient::new(virtual_handle)),
        }
    }
}

#[derive(Clone)]
enum FakeRunnerBehavior {
    PanicOnBoundaryUse,
    Discover(TransportValue),
    DiscoverAndRecordRuns {
        discover: TransportValue,
        requests: Arc<Mutex<Vec<RunRequest>>>,
        run_result: RunResult,
    },
}

#[derive(Clone)]
struct FakeRunner {
    behavior: FakeRunnerBehavior,
}

impl GracefulShutdown for FakeRunner {
    async fn graceful_shutdown(&self) {}
}

impl Runner for FakeRunner {
    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "fake-vx-runner".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: RUNNER_PROTOCOL_VERSION,
        }
    }

    async fn run(&self, _request: RunRequest, _progress: roam::Tx<StoreProgress>) -> RunResult {
        match &self.behavior {
            FakeRunnerBehavior::PanicOnBoundaryUse => {
                panic!("fake runner run should not be used in daemon-only build.vx tests");
            }
            FakeRunnerBehavior::Discover(_) => RunResult {
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: "fake runner was unexpectedly asked to run a build.vx exec request"
                    .to_string(),
                duration_ms: 0,
                observer_value: None,
                poison: None,
                error_kind: None,
                error: Some(
                    "fake runner was unexpectedly asked to run a build.vx exec request".to_string(),
                ),
            },
            FakeRunnerBehavior::DiscoverAndRecordRuns {
                requests,
                run_result,
                ..
            } => {
                requests.lock().expect("requests lock").push(_request);
                run_result.clone()
            }
        }
    }

    async fn discover(&self, _request: DiscoverRequest) -> DiscoverResult {
        match &self.behavior {
            FakeRunnerBehavior::PanicOnBoundaryUse => {
                panic!("fake runner discover should not be used in daemon-only build.vx tests");
            }
            FakeRunnerBehavior::Discover(value) => DiscoverResult {
                value: Some(value.clone()),
                poison: None,
            },
            FakeRunnerBehavior::DiscoverAndRecordRuns { discover, .. } => DiscoverResult {
                value: Some(discover.clone()),
                poison: None,
            },
        }
    }
}

#[derive(Clone)]
struct FakeRunnerVirtualAcceptor {
    boot_service: vx_io::boot::BootService<FakeRunner>,
}

impl ConnectionAcceptor for FakeRunnerVirtualAcceptor {
    fn accept(
        &self,
        _conn_id: roam_types::ConnectionId,
        peer_settings: &ConnectionSettings,
        _metadata: &[roam_types::MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        let Some(runner) = self.boot_service.gate().service().cloned() else {
            return Err(vec![]);
        };

        let parity = peer_settings.parity.other();
        Ok(AcceptedConnection {
            settings: ConnectionSettings {
                parity,
                max_concurrent_requests: peer_settings.max_concurrent_requests,
            },
            metadata: vec![],
            setup: Box::new(move |handle| {
                let dispatcher = RunnerDispatcher::new(runner);
                moire::task::spawn(
                    async move {
                        let mut driver = Driver::new(handle, dispatcher);
                        driver.run().await;
                    }
                    .named("fake_runner_virtual_driver"),
                );
            }),
        })
    }
}

struct FakeRunnerHandle {
    endpoint: vx_io::net::Endpoint,
    task: tokio::task::JoinHandle<()>,
}

impl FakeRunnerHandle {
    async fn new(behavior: FakeRunnerBehavior) -> Self {
        let endpoint = free_local_endpoint();
        let mut listener = Listener::bind(&endpoint)
            .await
            .expect("failed to bind fake runner endpoint");
        let (gate, boot_tx) = BootGate::<FakeRunner>::new();
        gate.set_service(FakeRunner { behavior });
        boot_tx.ready();
        let boot_service = gate.boot_service();

        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener
                    .accept()
                    .await
                    .expect("fake runner listener should accept");
                let boot_service = boot_service.clone();
                moire::task::spawn(
                    async move {
                        let (reader, writer) = tokio::io::split(stream);
                        let conduit = BareConduit::<roam_types::MessageFamily, _>::new(
                            StreamLink::new(reader, writer),
                        );
                        let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                        let (caller, session_handle) = acceptor(conduit)
                            .on_connection(FakeRunnerVirtualAcceptor { boot_service })
                            .establish::<roam_core::NoopCaller>(boot_dispatcher)
                            .await
                            .expect("fake runner handshake should succeed");
                        vx_io::net::park_session(caller, session_handle).await;
                    }
                    .named("fake_runner_connection"),
                );
            }
        });

        Self { endpoint, task }
    }

    async fn connect(&self) -> ServiceHandle {
        ServiceBuilder::new("fake-vx-runner", self.endpoint.clone())
            .caller_name("vxd-build-vx-daemon-tests")
            .timeout(std::time::Duration::from_secs(10))
            .connect_only()
            .connect()
            .await
            .expect("failed to connect to fake runner")
    }
}

impl Drop for FakeRunnerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct BuildServiceDaemonTestEnv {
    _store_root: TempDir,
    _vx_home: TempDir,
    _binaries: TestServiceBinaries,
    _lock: TestFileLock,
    _store_handle: Arc<ServiceHandle>,
    _fake_runner: FakeRunnerHandle,
    _runner_handle: Arc<ServiceHandle>,
    service: VxdService,
}

impl BuildServiceDaemonTestEnv {
    async fn new() -> Self {
        Self::with_build_vx_executor_factory(
            FakeRunnerBehavior::PanicOnBoundaryUse,
            crate::service::BuildVxExecutors::local_default,
        )
        .await
    }

    async fn with_build_vx_executor_factory<F>(
        fake_runner_behavior: FakeRunnerBehavior,
        build_vx_executors: F,
    ) -> Self
    where
        F: FnOnce(Arc<StoreClient>, Arc<RunnerClient>) -> crate::service::BuildVxExecutors,
    {
        let lock = lock_test_file("build-service");
        let binaries = copy_test_service_binaries();
        let store_root = TempDir::new().expect("failed to create store root tempdir");
        let vx_home = TempDir::new().expect("failed to create vx_home tempdir");
        let store_endpoint = free_local_endpoint();
        let store_root_str = store_root.path().to_string_lossy().into_owned();

        let store_handle = ServiceBuilder::new(&binaries.store, store_endpoint.clone())
            .caller_name("vxd-build-vx-tests")
            .args(&["--root", &store_root_str])
            .env("VX_TEST", "1")
            .timeout(std::time::Duration::from_secs(10))
            .connect()
            .await
            .expect("failed to spawn/connect vx-store");

        let fake_runner = FakeRunnerHandle::new(fake_runner_behavior).await;
        let runner_handle = fake_runner.connect().await;

        let (store_virtual, runner_virtual) = tokio::try_join!(
            store_handle.boot_and_connect(),
            runner_handle.boot_and_connect(),
        )
        .expect("failed to boot/connect store and runner");

        let store_handle = Arc::new(store_handle);
        let runner_handle = Arc::new(runner_handle);
        let store_client = Arc::new(StoreClient::new(store_virtual));
        let runner_client = Arc::new(RunnerClient::new(runner_virtual));
        let build_vx_executors =
            build_vx_executors(Arc::clone(&store_client), Arc::clone(&runner_client));

        let service = VxdService::new(
            Utf8Path::from_path(vx_home.path())
                .expect("vx_home path should be valid UTF-8")
                .to_owned(),
            "test-host".to_string(),
            crate::service::VxdServiceDeps {
                spawn_tracker: Arc::new(SyncMutex::new(
                    "vxd-build-vx-tests/spawn_tracker",
                    SpawnTracker::default(),
                )),
                store: store_client,
                build_vx_executors,
                store_handle: Arc::clone(&store_handle),
                activity_registry: crate::activity::ActivityRegistry::new(),
                build_vx_config_path: None,
            },
        )
        .await;

        Self {
            _store_root: store_root,
            _vx_home: vx_home,
            _binaries: binaries,
            _lock: lock,
            _store_handle: store_handle,
            _fake_runner: fake_runner,
            _runner_handle: runner_handle,
            service,
        }
    }
}

#[vx_test]
async fn build_vx_local_executor_is_lazy_and_reused() {
    let fake_runner = FakeRunnerHandle::new(FakeRunnerBehavior::PanicOnBoundaryUse).await;
    let runner_handle = fake_runner.connect().await;
    let runner_virtual = runner_handle
        .boot_and_connect()
        .await
        .expect("failed to boot/connect fake runner");
    let runner_client = Arc::new(RunnerClient::new(runner_virtual));
    let syntheses = Arc::new(AtomicUsize::new(0));

    let executors = crate::service::BuildVxExecutors::local_lazy({
        let syntheses = Arc::clone(&syntheses);
        let runner_client = Arc::clone(&runner_client);
        move |_listener| {
            let syntheses = Arc::clone(&syntheses);
            let runner_client = Arc::clone(&runner_client);
            Box::pin(async move {
                syntheses.fetch_add(1, Ordering::SeqCst);
                Ok(crate::service::BuildVxExecutor::local(runner_client, None))
            })
        }
    });

    let selector = vx_eval::ExecutorSelector::class("default");
    let runner_a = executors
        .runner_for(&selector, None)
        .await
        .expect("default executor should synthesize");
    let runner_b = executors
        .runner_for(&selector, None)
        .await
        .expect("default executor should be reused");

    assert_eq!(syntheses.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&runner_a, &runner_b));
}

#[vx_test]
async fn build_vx_local_executor_singleflights_concurrent_boot() {
    let fake_runner = FakeRunnerHandle::new(FakeRunnerBehavior::PanicOnBoundaryUse).await;
    let runner_handle = fake_runner.connect().await;
    let runner_virtual = runner_handle
        .boot_and_connect()
        .await
        .expect("failed to boot/connect fake runner");
    let runner_client = Arc::new(RunnerClient::new(runner_virtual));
    let syntheses = Arc::new(AtomicUsize::new(0));

    let executors = crate::service::BuildVxExecutors::local_lazy({
        let syntheses = Arc::clone(&syntheses);
        let runner_client = Arc::clone(&runner_client);
        move |_listener| {
            let syntheses = Arc::clone(&syntheses);
            let runner_client = Arc::clone(&runner_client);
            Box::pin(async move {
                syntheses.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok(crate::service::BuildVxExecutor::local(runner_client, None))
            })
        }
    });

    let selector = vx_eval::ExecutorSelector::class("default");
    let (runner_a, runner_b) = tokio::join!(
        executors.runner_for(&selector, None),
        executors.runner_for(&selector, None)
    );

    let runner_a = runner_a.expect("first default executor request should succeed");
    let runner_b = runner_b.expect("second default executor request should succeed");

    assert_eq!(syntheses.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&runner_a, &runner_b));
}

#[vx_test]
async fn build_vx_implicit_ssh_executor_does_not_synthesize_local_runner() {
    let local_syntheses = Arc::new(AtomicUsize::new(0));
    let ssh_syntheses = Arc::new(AtomicUsize::new(0));

    let executors = crate::service::BuildVxExecutors::local_lazy({
        let local_syntheses = Arc::clone(&local_syntheses);
        move |_listener| {
            let local_syntheses = Arc::clone(&local_syntheses);
            Box::pin(async move {
                local_syntheses.fetch_add(1, Ordering::SeqCst);
                Err("local executor should stay lazy".to_string())
            })
        }
    })
    .with_ssh_bootstrap_factory({
        let ssh_syntheses = Arc::clone(&ssh_syntheses);
        move |class_label, _target, _listener| {
            let ssh_syntheses = Arc::clone(&ssh_syntheses);
            Box::pin(async move {
                ssh_syntheses.fetch_add(1, Ordering::SeqCst);
                let fake_runner =
                    FakeRunnerHandle::new(FakeRunnerBehavior::PanicOnBoundaryUse).await;
                let runner_handle = Arc::new(fake_runner.connect().await);
                let runner_virtual = runner_handle
                    .boot_and_connect()
                    .await
                    .expect("failed to boot/connect fake runner");
                std::mem::forget(fake_runner);
                Ok(crate::service::SshBootstrapExecutor {
                    id: format!("ssh_bootstrap:{class_label}"),
                    backend: vx_eval::ExecutorBackendKind::SshBootstrap,
                    runner: Arc::new(RunnerClient::new(runner_virtual)),
                    store: None,
                    store_supports_readthrough: false,
                    runner_handle: Some(runner_handle),
                    store_handle: None,
                })
            })
        }
    });

    let selector = vx_eval::ExecutorSelector::class("ssh:toast");
    executors
        .runner_for(&selector, None)
        .await
        .expect("ssh executor should synthesize without local runner");

    assert_eq!(local_syntheses.load(Ordering::SeqCst), 0);
    assert_eq!(ssh_syntheses.load(Ordering::SeqCst), 1);
}

#[vx_test]
async fn acquire_blob_reuses_the_persistent_store_cache() {
    let env = StoreTestEnv::new().await;
    let body = b"hello acquire cache".to_vec();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&body)));
    let server = StaticHttpServer::new(body.clone(), "application/octet-stream");
    let request = EvalAcquireRequest {
        url: server.url("/blob.txt"),
        digest,
        extract: None,
        demand_chain: DemandChain::new(),
    };

    let first = crate::service::run_build_vx_acquire_via_store(&env.client, request.clone(), None)
        .await
        .expect("first acquire should succeed");
    let second = crate::service::run_build_vx_acquire_via_store(&env.client, request, None)
        .await
        .expect("second acquire should succeed");

    assert_eq!(server.hit_count(), 1, "expected one network fetch");
    assert!(matches!(
        first,
        Value::Blob(BlobValue::Store {
            executable: false,
            ..
        })
    ));
    assert!(matches!(
        second,
        Value::Blob(BlobValue::Store {
            executable: false,
            ..
        })
    ));
}

#[vx_test]
async fn acquire_extract_reuses_the_persistent_store_cache() {
    let env = StoreTestEnv::new().await;
    let archive = tar_gz_with_single_root(&[
        ("pkg-1.0/README.txt", b"hello from archive"),
        ("pkg-1.0/src/lib.c", b"int main(void) { return 0; }\n"),
    ]);
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&archive)));
    let server = StaticHttpServer::new(archive, "application/gzip");
    let request = EvalAcquireRequest {
        url: server.url("/pkg.tar.gz"),
        digest,
        extract: Some("tar.gz".to_string()),
        demand_chain: DemandChain::new(),
    };

    let first = crate::service::run_build_vx_acquire_via_store(&env.client, request.clone(), None)
        .await
        .expect("first extract acquire should succeed");
    let second = crate::service::run_build_vx_acquire_via_store(&env.client, request, None)
        .await
        .expect("second extract acquire should succeed");

    assert_eq!(server.hit_count(), 1, "expected one archive download");
    assert!(matches!(first, Value::Tree(TreeValue::Store(_))));
    assert!(matches!(second, Value::Tree(TreeValue::Store(_))));
}

#[vx_test]
async fn render_uses_workspace_relative_templates_and_refreshes_on_content_change() {
    let env = StoreTestEnv::new().await;
    let workspace = TempDir::new().expect("workspace tempdir");
    let workspace_root =
        Utf8Path::from_path(workspace.path()).expect("workspace path should be valid UTF-8");
    let template_dir = workspace_root.join("templates");
    fs::create_dir_all(&template_dir).expect("create template dir");
    let template_path = template_dir.join("header.txt");
    fs::write(
        &template_path,
        "name={{NAME}}\n{{#if ENABLED}}enabled\n{{/if}}{{#each FLAGS}}- {{this}}\n{{/each}}",
    )
    .expect("write template");

    let bindings = Value::Map(MapValue {
        entries: Arc::from([
            (
                Value::String(Arc::from("NAME")),
                Value::String(Arc::from("libpng")),
            ),
            (Value::String(Arc::from("ENABLED")), Value::Bool(true)),
            (
                Value::String(Arc::from("FLAGS")),
                Value::Array(Arc::from([
                    Value::String(Arc::from("PNG_SHARED")),
                    Value::String(Arc::from("PNG_STATIC")),
                ])),
            ),
        ]),
    });
    let request = EvalRenderRequest {
        template: "templates/header.txt".to_string(),
        bindings: bindings.clone(),
        demand_chain: DemandChain::new(),
    };

    let first =
        crate::service::run_build_vx_render_via_store(&env.client, workspace_root, request.clone())
            .await
            .expect("first render should succeed");
    assert_eq!(
        blob_text(&first),
        "name=libpng\nenabled\n- PNG_SHARED\n- PNG_STATIC\n"
    );

    fs::write(&template_path, "short={{NAME}}\n").expect("rewrite template");

    let second =
        crate::service::run_build_vx_render_via_store(&env.client, workspace_root, request)
            .await
            .expect("second render should succeed");
    assert_eq!(blob_text(&second), "short=libpng\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_vx_reports_unknown_executor_selectors() {
    let env = BuildServiceDaemonTestEnv::new().await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  Tree %{
    @tool.txt => discover(executor_class("linux"), (host) => Blob("present")),
  }
}
"#,
    )
    .expect("write build.vx");

    let result = env
        .service
        .do_build(BuildRequest::new(workspace_root.to_owned()))
        .await
        .expect("build should complete with a structured failure");

    assert!(!result.success, "build unexpectedly succeeded");
    let failure = result
        .failure
        .as_ref()
        .expect("build should include a structured failure");
    assert!(
        failure
            .reports
            .iter()
            .flat_map(|report| report.notes.iter())
            .any(|note| note.contains("build.vx executor class:linux is not configured")),
        "unexpected build failure: {:?}",
        failure,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_vx_routes_configured_daemon_executor_classes() {
    let discovered = Value::Discovered(vx_eval::DiscoveredValue {
        value: Box::new(Value::Blob(BlobValue::from_string("present"))),
        obligations: Arc::from([]),
    });
    let transport = TransportValue::try_from(&discovered)
        .expect("discovered value should serialize for fake runner");

    let env = BuildServiceDaemonTestEnv::with_build_vx_executor_factory(
        FakeRunnerBehavior::Discover(transport),
        |store_client, runner_client| {
            let mut build_vx_executors = crate::service::BuildVxExecutors::local_default(
                store_client,
                Arc::clone(&runner_client),
            );
            build_vx_executors.register_class_runner(
                "linux",
                "daemon:linux",
                vx_eval::ExecutorBackendKind::Daemon,
                runner_client,
            );
            build_vx_executors
        },
    )
    .await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  Tree %{
    @tool.txt => discover(executor_class("linux"), (host) => host.read("/tmp/tool")),
  }
}
"#,
    )
    .expect("write build.vx");

    let result = env
        .service
        .do_build(BuildRequest::new(workspace_root.to_owned()))
        .await
        .expect("build should succeed");

    assert!(
        result.success,
        "build unexpectedly failed: {:?}",
        result.failure
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/tool.txt")).expect("output"),
        "present"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_vx_supports_field_access_on_discovered_structs_from_runner() {
    let discovered = Value::Struct(vx_eval::StructValue {
        type_name: Arc::from("Toolchain"),
        fields: Arc::from([
            (
                Arc::from("os"),
                Value::Discovered(vx_eval::DiscoveredValue {
                    value: Box::new(Value::String(Arc::from("linux"))),
                    obligations: Arc::from([vx_eval::HostObservation::Os {
                        value: "linux".to_string(),
                    }]),
                }),
            ),
            (
                Arc::from("arch"),
                Value::Discovered(vx_eval::DiscoveredValue {
                    value: Box::new(Value::String(Arc::from("aarch64"))),
                    obligations: Arc::from([vx_eval::HostObservation::Arch {
                        value: "aarch64".to_string(),
                    }]),
                }),
            ),
            (
                Arc::from("cc"),
                Value::Discovered(vx_eval::DiscoveredValue {
                    value: Box::new(Value::String(Arc::from("/usr/bin/cc"))),
                    obligations: Arc::from([vx_eval::HostObservation::Which {
                        name: "cc".to_string(),
                        expected_path: Some("/usr/bin/cc".to_string()),
                    }]),
                }),
            ),
            (
                Arc::from("archive"),
                Value::Discovered(vx_eval::DiscoveredValue {
                    value: Box::new(Value::String(Arc::from("/usr/bin/ar"))),
                    obligations: Arc::from([vx_eval::HostObservation::Which {
                        name: "ar".to_string(),
                        expected_path: Some("/usr/bin/ar".to_string()),
                    }]),
                }),
            ),
        ]),
    });
    let transport = TransportValue::try_from(&discovered)
        .expect("discovered struct should serialize for fake runner");

    let env = BuildServiceDaemonTestEnv::with_build_vx_executor_factory(
        FakeRunnerBehavior::Discover(transport),
        |store_client, runner_client| {
            let mut build_vx_executors = crate::service::BuildVxExecutors::local_default(
                store_client,
                Arc::clone(&runner_client),
            );
            build_vx_executors.register_class_runner(
                "linux",
                "daemon:linux",
                vx_eval::ExecutorBackendKind::Daemon,
                runner_client,
            );
            build_vx_executors
        },
    )
    .await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
struct Toolchain {
  os: String,
  arch: String,
  cc: String,
  archive: String,
}

fn discover_toolchain(class: ExecutorClass) {
  discover(class, (host) => Toolchain {
    os: host.os(),
    arch: host.arch(),
    cc: host.which("cc"),
    archive: host.which("ar"),
  })
}

fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  let tc = discover_toolchain(executor_class("linux"));
  if tc.arch == "aarch64" (
    Tree %{ @tool.txt => Blob("arm") }
  ) else (
    Tree %{ @tool.txt => Blob("other") }
  )
}
"#,
    )
    .expect("write build.vx");

    let result = env
        .service
        .do_build(BuildRequest::new(workspace_root.to_owned()))
        .await
        .expect("build should succeed");

    assert!(
        result.success,
        "build unexpectedly failed: {:?}",
        result.failure
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/tool.txt")).expect("output"),
        "arm"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_vx_routes_implicit_ssh_executor_classes() {
    let discovered = Value::Discovered(vx_eval::DiscoveredValue {
        value: Box::new(Value::Blob(BlobValue::from_string("present"))),
        obligations: Arc::from([]),
    });
    let transport = TransportValue::try_from(&discovered)
        .expect("discovered value should serialize for fake runner");
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));

    let env = BuildServiceDaemonTestEnv::with_build_vx_executor_factory(
        FakeRunnerBehavior::PanicOnBoundaryUse,
        {
            let seen = Arc::clone(&seen);
            move |store_client, runner_client| {
                crate::service::BuildVxExecutors::local_default(store_client, runner_client)
                    .with_ssh_bootstrap_factory({
                        let seen = Arc::clone(&seen);
                        let transport = transport.clone();
                        move |class_label, target, _listener| {
                            let seen = Arc::clone(&seen);
                            let transport = transport.clone();
                            Box::pin(async move {
                                seen.lock()
                                    .expect("seen lock")
                                    .push((class_label.clone(), target));
                                let fake_runner =
                                    FakeRunnerHandle::new(FakeRunnerBehavior::Discover(transport))
                                        .await;
                                let runner_handle = Arc::new(fake_runner.connect().await);
                                let runner_virtual = runner_handle
                                    .boot_and_connect()
                                    .await
                                    .expect("failed to boot/connect fake runner");
                                std::mem::forget(fake_runner);
                                Ok(crate::service::SshBootstrapExecutor {
                                    id: format!("ssh_bootstrap:{class_label}"),
                                    backend: vx_eval::ExecutorBackendKind::SshBootstrap,
                                    runner: Arc::new(RunnerClient::new(runner_virtual)),
                                    store: None,
                                    store_supports_readthrough: false,
                                    runner_handle: Some(runner_handle),
                                    store_handle: None,
                                })
                            })
                        }
                    })
            }
        },
    )
    .await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  Tree %{
    @tool.txt => discover(executor_class("ssh:toast"), (host) => host.read("C:/Windows/win.ini")),
  }
}
"#,
    )
    .expect("write build.vx");

    let result = env
        .service
        .do_build(BuildRequest::new(workspace_root.to_owned()))
        .await
        .expect("build should succeed");

    assert!(
        result.success,
        "build unexpectedly failed: {:?}",
        result.failure
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/tool.txt")).expect("output"),
        "present"
    );
    assert_eq!(
        seen.lock().expect("seen lock").as_slice(),
        &[("ssh:toast".to_string(), "toast".to_string())]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_vx_accepts_discovered_top_level_trees() {
    let discovered = Value::Discovered(vx_eval::DiscoveredValue {
        value: Box::new(Value::Tree(TreeValue::inline(Arc::from([(
            Arc::from("tool.txt"),
            Value::Blob(BlobValue::from_string("present")),
        )])))),
        obligations: Arc::from([]),
    });
    let transport = TransportValue::try_from(&discovered)
        .expect("discovered tree should serialize for fake runner");

    let env = BuildServiceDaemonTestEnv::with_build_vx_executor_factory(
        FakeRunnerBehavior::Discover(transport),
        |store_client, runner_client| {
            let mut build_vx_executors = crate::service::BuildVxExecutors::local_default(
                store_client,
                Arc::clone(&runner_client),
            );
            build_vx_executors.register_class_runner(
                "linux",
                "daemon:linux",
                vx_eval::ExecutorBackendKind::Daemon,
                runner_client,
            );
            build_vx_executors
        },
    )
    .await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  discover(executor_class("linux"), (host) => Tree %{
    @tool.txt => host.read("/tmp/tool"),
  })
}
"#,
    )
    .expect("write build.vx");

    let result = env
        .service
        .do_build(BuildRequest::new(workspace_root.to_owned()))
        .await
        .expect("build should succeed");

    assert!(
        result.success,
        "build unexpectedly failed: {:?}",
        result.failure
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/tool.txt")).expect("output"),
        "present"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_vx_marks_discovered_exec_mounts_as_opaque() {
    let discovered = Value::Discovered(vx_eval::DiscoveredValue {
        value: Box::new(Value::Tree(TreeValue::inline(Arc::from([(
            Arc::from("cc"),
            Value::Blob(BlobValue::from_string("#!/bin/sh\nexit 0\n")),
        )])))),
        obligations: Arc::from([]),
    });
    let discover_transport = TransportValue::try_from(&discovered)
        .expect("discovered tree should serialize for fake runner");
    let run_observer_value =
        TransportValue::try_from(&Value::Tree(TreeValue::inline(Arc::from([(
            Arc::from("ok.txt"),
            Value::Blob(BlobValue::from_string("ok")),
        )]))))
        .expect("observer value should serialize");
    let requests = Arc::new(Mutex::new(Vec::<RunRequest>::new()));

    let env = BuildServiceDaemonTestEnv::with_build_vx_executor_factory(
        FakeRunnerBehavior::DiscoverAndRecordRuns {
            discover: discover_transport,
            requests: Arc::clone(&requests),
            run_result: RunResult {
                success: true,
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 0,
                observer_value: Some(run_observer_value),
                poison: None,
                error_kind: None,
                error: None,
            },
        },
        |store_client, runner_client| {
            let mut build_vx_executors = crate::service::BuildVxExecutors::local_default(
                store_client,
                Arc::clone(&runner_client),
            );
            build_vx_executors.register_class_runner(
                "linux",
                "daemon:linux",
                vx_eval::ExecutorBackendKind::Daemon,
                runner_client,
            );
            build_vx_executors
        },
    )
    .await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  let toolchain = discover(executor_class("linux"), (host) => Tree %{
    @cc => host.read("/tmp/tool"),
  });
  exec(["/bin/sh", "-c", "true"]) where {
    executor: executor_class("linux"),
    mounts: %{ @toolchain => toolchain },
    observer: (proc, out) => Tree %{
      @ok.txt => Blob("observer should have been bypassed by fake runner"),
    },
  }
}
"#,
    )
    .expect("write build.vx");

    let result = env
        .service
        .do_build(BuildRequest::new(workspace_root.to_owned()))
        .await
        .expect("build should succeed");

    assert!(
        result.success,
        "build unexpectedly failed: {:?}",
        result.failure
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/ok.txt")).expect("output"),
        "ok"
    );

    let requests = requests.lock().expect("requests lock");
    let request = requests
        .as_slice()
        .last()
        .expect("exec should have reached fake runner");
    assert_eq!(request.tree_mounts.len(), 1);
    assert_eq!(request.tree_mounts[0].mount, "toolchain");
    assert_eq!(request.tree_mounts[0].mode, RunMountMode::Opaque);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn build_vx_reports_unimplemented_ssh_bootstrap_executor_classes() {
    let env = BuildServiceDaemonTestEnv::with_build_vx_executor_factory(
        FakeRunnerBehavior::PanicOnBoundaryUse,
        |store_client, runner_client| {
            let mut build_vx_executors =
                crate::service::BuildVxExecutors::local_default(store_client, runner_client);
            build_vx_executors.register_class_unavailable(
                "linux",
                "ssh_bootstrap:linux",
                vx_eval::ExecutorBackendKind::SshBootstrap,
                "build.vx executor ssh_bootstrap:linux is configured for amos@example, but SSH bootstrap is not implemented yet",
            );
            build_vx_executors
        },
    )
    .await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  Tree %{
    @tool.txt => discover(executor_class("linux"), (host) => Blob("present")),
  }
}
"#,
    )
    .expect("write build.vx");

    let result = env
        .service
        .do_build(BuildRequest::new(workspace_root.to_owned()))
        .await
        .expect("build should complete with a structured failure");

    assert!(!result.success, "build unexpectedly succeeded");
    let failure = result
        .failure
        .as_ref()
        .expect("build should include a structured failure");
    assert!(
        failure
            .reports
            .iter()
            .flat_map(|report| report.notes.iter())
            .any(|note| note.contains("SSH bootstrap is not implemented yet")),
        "unexpected build failure: {:?}",
        failure,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stabilize_reuses_resolve_output_until_witness_changes() {
    let env = BuildServiceDaemonTestEnv::new().await;
    let workspace = TempDir::new().expect("workspace");
    let workspace_root = Utf8Path::from_path(workspace.path()).expect("utf8 workspace");

    fs::write(
        workspace.path().join("build.vx"),
        r#"
fn build(workspace: Path, args: Array[String], env: Map[String, String]) -> Tree (
  Tree(%{
    @out.txt => stabilize(
      () => Tree.from_path(workspace).file(@input.txt).utf8(),
      (value) => render(@template.txt) where {
        bindings: %{
          "VALUE" => value,
        },
      },
    ),
  })
)
"#,
    )
    .expect("write build.vx");
    fs::write(workspace.path().join("input.txt"), "world\n").expect("write input");
    fs::write(workspace.path().join("template.txt"), "hello {{VALUE}}").expect("write template");

    let request = BuildRequest::new(workspace_root.to_owned());

    let first = env
        .service
        .do_build(request.clone())
        .await
        .expect("first stabilize build should succeed");
    assert!(first.success, "first build failed: {:?}", first.failure);
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/out.txt")).expect("first output"),
        "hello world\n"
    );

    fs::write(workspace.path().join("template.txt"), "goodbye {{VALUE}}")
        .expect("rewrite template");

    let second = env
        .service
        .do_build(request.clone())
        .await
        .expect("second stabilize build should succeed");
    assert!(second.success, "second build failed: {:?}", second.failure);
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/out.txt")).expect("second output"),
        "hello world\n",
        "template-only changes should not invalidate stabilize"
    );

    fs::write(workspace.path().join("input.txt"), "mars\n").expect("rewrite input");

    let third = env
        .service
        .do_build(request)
        .await
        .expect("third stabilize build should succeed");
    assert!(third.success, "third build failed: {:?}", third.failure);
    assert_eq!(
        fs::read_to_string(workspace.path().join(".vx/build/out.txt")).expect("third output"),
        "goodbye mars\n"
    );
}

fn blob_text(value: &Value) -> String {
    let Value::Blob(blob) = value else {
        panic!("expected blob value, got {}", value.type_name());
    };
    blob.to_utf8_string()
        .expect("blob should be valid UTF-8")
        .to_string()
}

struct StaticHttpServer {
    addr: std::net::SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StaticHttpServer {
    fn new(body: Vec<u8>, content_type: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind test http server");
        let addr = listener
            .local_addr()
            .expect("failed to read test http server address");
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_for_thread = Arc::clone(&hits);
        let stop_for_thread = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if stop_for_thread.load(Ordering::Relaxed) {
                            break;
                        }
                        let mut request = [0u8; 1024];
                        let _ = stream.read(&mut request);
                        hits_for_thread.fetch_add(1, Ordering::Relaxed);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                            content_type,
                        );
                        stream
                            .write_all(response.as_bytes())
                            .expect("failed to write response headers");
                        stream
                            .write_all(&body)
                            .expect("failed to write response body");
                    }
                    Err(error) => panic!("test http server accept failed: {error}"),
                }
            }
        });

        Self {
            addr,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn hit_count(&self) -> usize {
        self.hits.load(Ordering::Relaxed)
    }
}

impl Drop for StaticHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("test http server thread should join");
        }
    }
}

fn tar_gz_with_single_root(files: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), GzipCompression::default());
    let mut builder = TarBuilder::new(encoder);

    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, *bytes)
            .expect("failed to append tar entry");
    }

    let encoder = builder.into_inner().expect("failed to finish tar archive");
    encoder.finish().expect("failed to finish gzip stream")
}

fn dir_entry_kind_from_blob_ref(blob_ref: &BlobRef, executable: bool) -> DirEntryKind {
    match blob_ref {
        BlobRef::Small { chunk, size } => DirEntryKind::SmallFile {
            content_hash: ContentHash(chunk.0),
            size: *size,
            executable,
        },
        BlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => DirEntryKind::LargeFile {
            blob_node: *blob_node,
            content_hash: *content_hash,
            size: *size,
            executable,
        },
    }
}

fn free_local_endpoint() -> vx_io::net::Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    let addr = listener
        .local_addr()
        .expect("failed to read ephemeral port");
    drop(listener);
    vx_io::net::Endpoint::parse(&addr.to_string()).expect("failed to parse test endpoint")
}

fn resolve_vx_store_binary() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/vx-store")
        .to_string_lossy()
        .into_owned()
}
