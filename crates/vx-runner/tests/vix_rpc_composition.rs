#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use vix::exec::{ExecEvent, Outcome, ReadObservation, TreeEvent};
use vix::machine::{
    DriveEvent, Machine, MachineExecBackend, MachinePathDemand, MachinePendingRun, RenderedValue,
};
use vix::rpc_process::RpcRunnerBackend;
use vx_exec::SandboxedExecutor;
use vx_services::RunnerDispatcher;

const SOURCE: &str = r#"
use vix::Tree;

pub fn run_script(script: Tree) -> String {
    let build_script = "build-script-runner";
    let run = build_script! {
        --executable {script / p"run.sh"}
        --stdout {p"build.stdout"}
        --out-dir {p"out"}
        --env CARGO_MANIFEST_DIR={script}
    };
    run.text(p"build.stdout")
}
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vix_rpc_backend_drives_real_runner_over_websocket() -> Result<(), String> {
    if !host_tools_available() {
        return Ok(());
    }

    let (endpoint, server) = serve_runner().await?;
    let backend = Arc::new(ObservingBackend::new(RpcRunnerBackend::new(endpoint)));
    let mut machine = Machine::load(SOURCE)?.with_exec_backend(backend.clone());
    let script = machine.intern_arg(
        "Tree",
        vix::machine::MachineArg::Tree(vix::exec::Tree::of(&[
            (
                "run.sh",
                "#!/bin/sh\nwhile IFS= read -r line; do printf '%s\\n' \"$line\"; done < \"$CARGO_MANIFEST_DIR/message.txt\"\n",
            ),
            ("message.txt", "hello from vix over rpc\n"),
        ])),
    )?;

    let output = machine.demand_i64("run_script", vec![script.0])?;
    assert_eq!(
        machine.render_result("run_script", output)?,
        RenderedValue::String {
            value: "hello from vix over rpc\n".to_string()
        }
    );

    let completed = run_completed(&machine)?;
    assert_eq!(completed.serving, ExecEvent::Ran);
    assert_eq!(
        completed.outputs,
        vec![(
            "build.stdout".to_string(),
            "hello from vix over rpc\n".to_string()
        )]
    );

    let outcomes = backend.outcomes()?;
    let outcome = outcomes
        .last()
        .ok_or_else(|| "rpc backend did not capture an outcome".to_string())?;
    assert!(matches!(
        outcome.read_set.entries.get("/m/0/run.sh"),
        Some(ReadObservation::Content(_))
    ));
    assert!(
        outcome
            .read_set
            .entries
            .values()
            .any(|observation| matches!(observation, ReadObservation::Listing(_))),
        "expected at least one directory listing observation, got {:?}",
        outcome.read_set
    );
    assert!(matches!(
        outcome.tree_events.as_slice(),
        [
            TreeEvent::SubfileCompleted(completed),
            TreeEvent::TreeFinalized { files }
        ] if completed.path == "build.stdout" && files.len() == 1
    ));

    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vix_rpc_backend_preserves_runner_typed_undeclared_read_failure() -> Result<(), String> {
    if !host_tools_available() {
        return Ok(());
    }

    let fixture = Fixture::new("vix-rpc-undeclared-read");
    let secret = fixture.root.join("secret.txt");
    fs::write(&secret, "not declared\n").map_err(|err| err.to_string())?;

    let (endpoint, server) = serve_runner().await?;
    let backend = Arc::new(ObservingBackend::new(RpcRunnerBackend::new(endpoint)));
    let mut machine = Machine::load(SOURCE)?.with_exec_backend(backend.clone());
    let script = machine.intern_arg(
        "Tree",
        vix::machine::MachineArg::Tree(vix::exec::Tree::of(&[(
            "run.sh",
            &format!(
                "#!/bin/sh\nwhile IFS= read -r line; do printf '%s\\n' \"$line\"; done < {}\n",
                shell_quote(&secret)
            ),
        )])),
    )?;

    let err = machine
        .demand_i64("run_script", vec![script.0])
        .expect_err("undeclared read must fail through vix rpc backend");
    assert!(
        err.starts_with("rpc runner process exit failure:")
            || err.starts_with("rpc runner spawn failure:"),
        "undeclared read did not preserve mapped typed runner failure: {err}"
    );
    assert_eq!(backend.errors()?, vec![err]);

    server.abort();
    Ok(())
}

async fn serve_runner() -> Result<(String, tokio::task::JoinHandle<()>), String> {
    let listener = vox::WsListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| format!("bind runner websocket listener: {err}"))?;
    let endpoint = format!(
        "ws://{}",
        listener
            .local_addr()
            .map_err(|err| format!("runner websocket local addr: {err}"))?
    );
    let server = tokio::spawn(async move {
        vox::serve_listener(listener, RunnerDispatcher::new(SandboxedExecutor::new()))
            .await
            .expect("serve runner websocket");
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Ok((endpoint, server))
}

#[derive(Clone)]
struct ObservingBackend {
    inner: Arc<RpcRunnerBackend>,
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl ObservingBackend {
    fn new(inner: RpcRunnerBackend) -> Self {
        Self {
            inner: Arc::new(inner),
            outcomes: Arc::new(Mutex::new(Vec::new())),
            errors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn outcomes(&self) -> Result<Vec<Outcome>, String> {
        self.outcomes
            .lock()
            .map_err(|_| "captured outcomes poisoned".to_string())
            .map(|outcomes| outcomes.clone())
    }

    fn errors(&self) -> Result<Vec<String>, String> {
        self.errors
            .lock()
            .map_err(|_| "captured errors poisoned".to_string())
            .map(|errors| errors.clone())
    }
}

impl MachineExecBackend for ObservingBackend {
    fn spawn(
        &self,
        request: vix::machine::MachineExecRequest,
    ) -> Result<Arc<dyn MachinePendingRun>, String> {
        Ok(Arc::new(ObservingRun {
            inner: self.inner.spawn(request)?,
            outcomes: Arc::clone(&self.outcomes),
            errors: Arc::clone(&self.errors),
        }))
    }
}

struct ObservingRun {
    inner: Arc<dyn MachinePendingRun>,
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl MachinePendingRun for ObservingRun {
    fn demand_path(&self, path: &str) -> Result<MachinePathDemand, String> {
        match self.inner.demand_path(path)? {
            MachinePathDemand::File(_) => Ok(MachinePathDemand::FinishRequired {
                path: path.to_string(),
            }),
            demand => Ok(demand),
        }
    }

    fn flush(&self) -> Result<(Outcome, ExecEvent), String> {
        match self.inner.flush() {
            Ok((outcome, event)) => {
                self.outcomes
                    .lock()
                    .map_err(|_| "captured outcomes poisoned".to_string())?
                    .push(outcome.clone());
                Ok((outcome, event))
            }
            Err(err) => {
                self.errors
                    .lock()
                    .map_err(|_| "captured errors poisoned".to_string())?
                    .push(err.clone());
                Err(err)
            }
        }
    }
}

#[derive(Debug)]
struct CompletedRun {
    serving: ExecEvent,
    outputs: Vec<(String, String)>,
}

fn run_completed(machine: &Machine) -> Result<CompletedRun, String> {
    let completed = machine
        .trace()
        .iter()
        .filter_map(|event| match event {
            DriveEvent::RunCompleted {
                serving, outputs, ..
            } => Some(CompletedRun {
                serving: serving.clone(),
                outputs: outputs.clone(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    match completed.as_slice() {
        [completed] => Ok(CompletedRun {
            serving: completed.serving.clone(),
            outputs: completed.outputs.clone(),
        }),
        _ => Err(format!("expected one completed run, got {completed:?}")),
    }
}

fn host_tools_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists() && Path::new("/bin/sh").exists()
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
        fs::create_dir_all(&root).expect("create fixture root");
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
        .expect("system time after unix epoch")
        .as_nanos()
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
