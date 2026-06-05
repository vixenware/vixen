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
    request: Arc<Mutex<Option<ObserveRequest>>>,
}

impl Observe for RecordingObserve {
    async fn observe(&self, request: ObserveRequest) -> Observed {
        *self.request.lock().unwrap() = Some(request);
        Observed::Value(WireValue::Unit)
    }
}

impl vox::ConnectionAcceptor for RecordingObserve {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), vox::Metadata<'static>> {
        match request.service() {
            "Observe" => {
                connection.handle_with(ObserveDispatcher::new(self.clone()));
                Ok(())
            }
            _ => Err(vec![]),
        }
    }
}

async fn recording_observe_client() -> (ObserveClient, Arc<Mutex<Option<ObserveRequest>>>) {
    let request = Arc::new(Mutex::new(None));
    let service = RecordingObserve {
        request: Arc::clone(&request),
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
    let client = vox::initiator_on(client_link, vox::TransportMode::Bare)
        .establish::<ObserveClient>()
        .await
        .expect("establish recording observe client");
    (client, request)
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
    let (client, request) = scheduler.block_on(recording_observe_client());
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
    let request = request
        .lock()
        .unwrap()
        .clone()
        .expect("observe request was dispatched");
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
    let (client, request) = scheduler.block_on(recording_observe_client());
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
    let request = request
        .lock()
        .unwrap()
        .clone()
        .expect("observe request was dispatched");
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
    let (client, request) = scheduler.block_on(recording_observe_client());
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
    let request = request
        .lock()
        .unwrap()
        .clone()
        .expect("observe request was dispatched");
    assert_eq!(request.mounts.len(), 1);
    assert_eq!(request.mounts[0].path, "src");
    assert_eq!(request.mounts[0].tree.bytes, tree_hash);
}
