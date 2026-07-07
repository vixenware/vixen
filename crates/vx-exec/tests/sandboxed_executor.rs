#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};

use vx_exec::SandboxedExecutor;
use vx_services::{
    Blake3Hash, ContentHash, ExecArg, ExecMount, ExecPlan, ExecReadObservation, ExecRequest,
    ExecRole, ExecTree, Runner,
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
            program: "/bin/sh".to_string(),
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
                at: "/m/in".to_string(),
                tree: input_tree,
            }],
            toolchain_roots: default_toolchain_roots(),
        })
        .await;

    assert_eq!(result.exit_code, 0, "stderr: {}", stderr(&result));
    assert_eq!(result.error, None);
    let outcome = result.outcome.expect("successful exec returns an outcome");
    assert_eq!(
        outcome.outputs.entries.get("/m/out/hello.txt"),
        Some(&"hello from declared mount\n".to_string())
    );
    assert_eq!(
        outcome.read_set.entries.get("/m/in/message.txt"),
        Some(&ExecReadObservation::Content(ContentHash(
            Blake3Hash::from_bytes(b"hello from declared mount\n")
        )))
    );
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
        result.exit_code, 0,
        "undeclared read unexpectedly succeeded"
    );
    assert!(
        result.outcome.is_none(),
        "failed exec must not harvest outputs"
    );
    assert!(
        result.error.as_deref().is_some_and(|error| {
            error.contains("sandboxed exec") || error.contains("Operation not permitted")
        }),
        "failure should be machine-visible: {:?}\nstderr:\n{}",
        result.error,
        stderr(&result)
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
        result.exit_code, 0,
        "undeclared write unexpectedly succeeded"
    );
    assert!(
        result.outcome.is_none(),
        "failed exec must not harvest outputs"
    );
    assert!(!outside.exists(), "sandbox denial must prevent the write");
    assert!(
        result.error.is_some(),
        "failure should be machine-visible\nstderr:\n{}",
        stderr(&result)
    );
}

fn shell_request(script: &str, output: &str) -> ExecRequest {
    ExecRequest {
        program: "/bin/sh".to_string(),
        plan: ExecPlan {
            argv: vec![
                ExecArg {
                    value: "-c".to_string(),
                    role: ExecRole::Flag,
                },
                ExecArg {
                    value: script.to_string(),
                    role: ExecRole::Flag,
                },
                ExecArg {
                    value: format!("OUT={output}"),
                    role: ExecRole::Env,
                },
                ExecArg {
                    value: output.to_string(),
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
        value: value.to_string(),
        role,
    }
}

fn default_toolchain_roots() -> Vec<String> {
    vec!["/bin".to_string(), "/usr/bin".to_string()]
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
