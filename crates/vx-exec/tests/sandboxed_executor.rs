#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};

use vx_exec::SandboxedExecutor;
use vx_services::{
    Blake3Hash, ByteLen, ContentHash, ExecArg, ExecArgValue, ExecCompletion, ExecError,
    ExecErrorKind, ExecExitCode, ExecMount, ExecOutcome, ExecPath, ExecPlan, ExecProgram,
    ExecReadObservation, ExecRequest, ExecRole, ExecTree, ExecTreeEvent, Runner,
};

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
        Some(&"hello from declared mount\n".to_string())
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
        Some(&"observed\n".to_string())
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
