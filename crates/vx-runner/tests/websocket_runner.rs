#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};

use vx_exec::SandboxedExecutor;
use vx_services::{
    ByteLen, CapabilityProbeRequest, ExecArg, ExecArgValue, ExecBlake3Hash as Blake3Hash,
    ExecCompletion, ExecContentHash as ContentHash, ExecErrorKind, ExecExitCode, ExecMount,
    ExecPath, ExecPlan, ExecProgram, ExecReadObservation, ExecRequest, ExecRole, ExecText,
    ExecTree, ExecTreeEvent, RunnerClient, RunnerDispatcher, RunnerTransport, SandboxCapability,
};

#[tokio::test]
async fn websocket_runner_exec_returns_observed_outcome_and_typed_failure() {
    if !host_tools_available() {
        return;
    }

    let listener = vox::WsListener::bind("127.0.0.1:0")
        .await
        .expect("bind runner websocket listener");
    let addr = listener.local_addr().expect("runner websocket local addr");
    let server = tokio::spawn(async move {
        vox::serve_listener(listener, RunnerDispatcher::new(SandboxedExecutor::new()))
            .await
            .expect("serve runner websocket");
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client: RunnerClient = vox::connect_lane(format!("ws://{addr}"))
        .await
        .expect("connect runner client");

    let capabilities = client
        .capabilities(CapabilityProbeRequest {})
        .await
        .expect("runner capabilities")
        .capabilities;
    assert!(
        capabilities
            .transports
            .contains(&RunnerTransport::VoxWebsocket)
    );
    assert!(
        capabilities
            .sandboxes
            .contains(&SandboxCapability::Seatbelt)
    );

    let result = client
        .exec(shell_copy_request())
        .await
        .expect("runner exec rpc");
    assert_eq!(
        result.exit_code,
        ExecExitCode(0),
        "stderr: {}",
        stderr(&result)
    );
    let outcome = match result.completion {
        ExecCompletion::Succeeded(outcome) => outcome,
        ExecCompletion::Failed(error) => panic!("expected success, got {error:?}"),
    };
    assert_eq!(
        outcome.outputs.entries.get("/m/out/hello.txt"),
        Some(&ExecText::from("hello over vox\n"))
    );
    assert_eq!(
        outcome.read_set.entries.get("/m/in/message.txt"),
        Some(&ExecReadObservation::File {
            content_hash: ContentHash(Blake3Hash::from_bytes(b"hello over vox\n")),
            blob_node: None,
            size: ByteLen("hello over vox\n".len() as u64),
        })
    );
    assert!(
        outcome
            .tree_events
            .iter()
            .any(|event| matches!(event, ExecTreeEvent::SubfileCompleted(file) if file.path == ExecPath::from("/m/out/hello.txt")))
    );

    let fixture = Fixture::new("runner-undeclared-read");
    let secret = fixture.root.join("secret.txt");
    fs::write(&secret, "not declared\n").unwrap();
    let failure = client
        .exec(shell_request(
            &format!("cat {} > \"$OUT\"", shell_quote(&secret)),
            "/m/out/read.txt",
        ))
        .await
        .expect("runner failure rpc");
    let error = match failure.completion {
        ExecCompletion::Failed(error) => error,
        ExecCompletion::Succeeded(_) => panic!("undeclared read unexpectedly succeeded"),
    };
    assert!(
        error.kind == ExecErrorKind::ProcessExit || error.kind == ExecErrorKind::Spawn,
        "undeclared read must cross RPC as typed failure, got {error:?}"
    );

    server.abort();
}

fn shell_copy_request() -> ExecRequest {
    let mut input_tree = ExecTree::default();
    input_tree.insert_bytes("message.txt", b"hello over vox\n".to_vec());

    ExecRequest {
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
    }
}

fn shell_request(script: &str, output: &str) -> ExecRequest {
    ExecRequest {
        program: ExecProgram::from("/bin/sh"),
        plan: ExecPlan {
            argv: vec![
                arg("-c", ExecRole::Flag),
                arg(script, ExecRole::Flag),
                arg(&format!("OUT={output}"), ExecRole::Env),
                arg(output, ExecRole::Output),
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

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "vx-runner-{name}-{}-{}",
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
