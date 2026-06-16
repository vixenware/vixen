use std::sync::{Arc, Mutex};

use vx_exec_protocol::{
    Observe, ObserveClient, ObserveDispatcher, ObserveRequest, Observed, WireValue,
};
use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Value, eval_body_with_exec, tool_value,
};
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

#[derive(Clone)]
struct RecordingObserve {
    requests: Arc<Mutex<Vec<ObserveRequest>>>,
    observed: WireValue,
}

impl Observe for RecordingObserve {
    async fn observe(&self, request: ObserveRequest) -> Observed {
        self.requests.lock().unwrap().push(request);
        Observed::Value(self.observed.clone())
    }
}

impl vox::ConnectionRouter for RecordingObserve {
    fn route(
        &self,
        request: &vox::ConnectionRequest,
    ) -> std::result::Result<vox::ConnectionRoute, vox::Metadata> {
        match request.service() {
            "Observe" => Ok(vox::ConnectionRoute::handle(ObserveDispatcher::new(
                self.clone(),
            ))),
            _ => Err(vox::Metadata::default()),
        }
    }
}

async fn recording_observe_client() -> (ObserveClient, Arc<Mutex<Vec<ObserveRequest>>>) {
    recording_observe_client_with(WireValue::Unit).await
}

async fn recording_observe_client_with(
    observed: WireValue,
) -> (ObserveClient, Arc<Mutex<Vec<ObserveRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let service = RecordingObserve {
        requests: Arc::clone(&requests),
        observed,
    };
    let (client_link, server_link) = vox::memory_link_pair(64);
    tokio::spawn(async move {
        if let Ok(client) = vox::acceptor_on(server_link)
            .on_connection(service)
            .establish::<vox::NoopClient>()
            .await
        {
            client.caller.closed().await;
        }
    });
    let client = vox::initiator_on(client_link)
        .establish::<ObserveClient>()
        .await
        .expect("establish recording observe client");
    (client, requests)
}

fn take_one_request(requests: &Arc<Mutex<Vec<ObserveRequest>>>) -> ObserveRequest {
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one observe request, got {}",
        requests.len()
    );
    requests[0].clone()
}

fn entry_body(fe: &vx_runtime::compile::FrontEnd, name: &str) -> vx_ir::ids::BodyId {
    fe.program
        .arena
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ItemKind::Function(function) if function.name == name => Some(function.body),
            _ => None,
        })
        .expect("entry body")
}

#[test]
fn demanded_exec_forwards_create_dirs_to_observe_request() {
    let fe = compile_source(
        "fn t(cmd: Command) -> () ( \
           `$cmd` with { \
             writable: [\"out\"], \
             create_dirs: [\"out/obj\"], \
             observer: (run) => () \
           } \
         )",
        "t",
    )
    .expect("exec create_dirs program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, requests) = scheduler.block_on(recording_observe_client());
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid) {
            EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected unit observe value, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    let request = take_one_request(&requests);
    assert_eq!(request.outputs, vec!["out".to_string()]);
    assert_eq!(request.create_dirs, vec!["out/obj".to_string()]);
}

#[test]
fn demanded_exec_forwards_env_to_observe_request() {
    let fe = compile_source(
        "fn t(cmd: Command, key: String, value: String) -> () ( \
           `$cmd` with { \
             env: %{ key => value }, \
             observer: (run) => () \
           } \
         )",
        "t",
    )
    .expect("exec env program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, requests) = scheduler.block_on(recording_observe_client());
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        let key = Value::Str("VX_ENV_PROOF".into());
        let value = Value::Str("yes".into());
        match eval_body_with_exec(
            &program,
            &env,
            entry,
            vec![cmd, key, value],
            KernelMode::GraphOnly,
            cid,
        ) {
            EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected unit observe value, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    let request = take_one_request(&requests);
    assert_eq!(request.env.len(), 1);
    assert_eq!(request.env[0].key, "VX_ENV_PROOF");
    assert_eq!(request.env[0].value, "yes");
}

#[test]
fn demanded_exec_forwards_mounts_to_observe_request() {
    let fe = compile_source(
        "fn t(cmd: Command, tree: Tree) -> () ( \
           `$cmd` with { \
             mounts: %{ \"src\" => tree }, \
             observer: (run) => () \
           } \
         )",
        "t",
    )
    .expect("exec mounts program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, requests) = scheduler.block_on(recording_observe_client());
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let tree_hash = [0x42; 32];
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        let tree = Value::CasTree(tree_hash);
        match eval_body_with_exec(
            &program,
            &env,
            entry,
            vec![cmd, tree],
            KernelMode::GraphOnly,
            cid,
        ) {
            EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected unit observe value, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    let request = take_one_request(&requests);
    assert_eq!(request.mounts.len(), 1);
    assert_eq!(request.mounts[0].path, "src");
    assert_eq!(request.mounts[0].tree.bytes, tree_hash);
}

#[test]
fn identical_demanded_exec_boundaries_share_runtime_l1_without_live_vfs() {
    let fe = compile_source(
        "fn t(cmd: Command) -> Bool ( \
           let first = `$cmd` with { observer: (run) => \"ok\" }; \
           let second = `$cmd` with { observer: (run) => \"ok\" }; \
           first == second \
         )",
        "t",
    )
    .expect("exec cache program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, requests) = scheduler.block_on(recording_observe_client_with(WireValue::Str(
        "ok".to_string(),
    )));
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid) {
            EvalOutcome::Clean(Value::Bool(true)) => Outcome::Clean,
            other => Outcome::Poisoned(format!(
                "expected identical exec boundaries to compare equal, got {other:?}"
            )),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "the second identical demanded exec must reuse the runtime L1 observe result"
    );
}

#[test]
fn exec_env_entries_disambiguate_runtime_l1_without_live_vfs() {
    let fe = compile_source(
        "fn t(cmd: Command) -> Bool ( \
           let first = `$cmd` with { env: %{ \"VX_ENV_PROOF\" => \"a\" }, observer: (run) => \"ok\" }; \
           let second = `$cmd` with { env: %{ \"VX_ENV_PROOF\" => \"b\" }, observer: (run) => \"ok\" }; \
           first == second \
         )",
        "t",
    )
    .expect("exec env-cache program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, requests) = scheduler.block_on(recording_observe_client_with(WireValue::Str(
        "ok".to_string(),
    )));
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid) {
            EvalOutcome::Clean(Value::Bool(true)) => Outcome::Clean,
            other => Outcome::Poisoned(format!(
                "expected env-distinct exec boundaries to compare equal, got {other:?}"
            )),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "different env entries must not alias in the runtime L1"
    );
}

#[test]
fn exec_mounts_disambiguate_runtime_l1_without_live_vfs() {
    let fe = compile_source(
        "fn t(cmd: Command, a: Tree, b: Tree) -> Bool ( \
           let first = `$cmd` with { mounts: %{ \"src\" => a }, observer: (run) => \"ok\" }; \
           let second = `$cmd` with { mounts: %{ \"src\" => b }, observer: (run) => \"ok\" }; \
           first == second \
         )",
        "t",
    )
    .expect("exec mount-cache program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, requests) = scheduler.block_on(recording_observe_client_with(WireValue::Str(
        "ok".to_string(),
    )));
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        let a = Value::CasTree([0x01; 32]);
        let b = Value::CasTree([0x02; 32]);
        match eval_body_with_exec(
            &program,
            &env,
            entry,
            vec![cmd, a, b],
            KernelMode::GraphOnly,
            cid,
        ) {
            EvalOutcome::Clean(Value::Bool(true)) => Outcome::Clean,
            other => Outcome::Poisoned(format!(
                "expected mount-distinct exec boundaries to compare equal, got {other:?}"
            )),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "different CAS tree mounts must not alias in the runtime L1"
    );
}

#[test]
fn exec_writable_and_create_dirs_disambiguate_runtime_l1_without_live_vfs() {
    let fe = compile_source(
        "fn t(cmd: Command) -> Bool ( \
           let first = `$cmd` with { \
             writable: [\"out-a\"], \
             create_dirs: [\"out-a/obj\"], \
             observer: (run) => \"ok\" \
           }; \
           let second = `$cmd` with { \
             writable: [\"out-b\"], \
             create_dirs: [\"out-b/obj\"], \
             observer: (run) => \"ok\" \
           }; \
           first == second \
         )",
        "t",
    )
    .expect("exec writable/create_dirs cache program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, requests) = scheduler.block_on(recording_observe_client_with(WireValue::Str(
        "ok".to_string(),
    )));
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid) {
            EvalOutcome::Clean(Value::Bool(true)) => Outcome::Clean,
            other => Outcome::Poisoned(format!(
                "expected output-shape-distinct exec boundaries to compare equal, got {other:?}"
            )),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "different writable/create_dirs entries must not alias in the runtime L1"
    );
}
