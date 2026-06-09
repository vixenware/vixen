use vx_exec_protocol::{
    Observe, ObserveClient, ObserveDispatcher, ObserveRequest, Observed, WireValue,
};
use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Value, eval_body_with_exec, tool_value,
};
use vx_runtime::remote::{self, Producer, ProducerService, ProducingDispatcher};
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

#[derive(Clone)]
struct NestedTreeObserve {
    producing: ProducerService,
}

impl Observe for NestedTreeObserve {
    async fn observe(&self, _request: ObserveRequest) -> Observed {
        Observed::ValueWithTree(WireValue::Struct(vec![(
            "out".to_string(),
            WireValue::Tree,
        )]))
    }
}

impl vox::ConnectionAcceptor for NestedTreeObserve {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), vox::Metadata> {
        match request.service() {
            "Observe" => {
                connection.handle_with(ObserveDispatcher::new(self.clone()));
                Ok(())
            }
            "Producing" => {
                connection.handle_with(ProducingDispatcher::new(self.producing.clone()));
                Ok(())
            }
            _ => Err(vox::Metadata::default()),
        }
    }
}

async fn nested_tree_observe_client() -> (ObserveClient, Producer) {
    let (producing, producer) = remote::producer();
    let service = NestedTreeObserve { producing };
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
        .expect("establish nested-tree observe client");
    (client, producer)
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
fn nested_observer_tree_field_crosses_as_remote_handle() {
    let fe = compile_source(
        "struct Nested { out: Tree }\n\
         fn t(cmd: Command) -> Blob ( \
           let nested = `$cmd` with { observer: (run) => Nested { out: run.stream.out } }; \
           nested.out.file(\"out\") \
         )",
        "t",
    )
    .expect("nested tree observer program compiles");
    let entry = entry_body(&fe, "t");

    let scheduler = Scheduler::new(&[1]);
    let (client, producer) = scheduler.block_on(nested_tree_observe_client());
    producer.write_path("out", b"nested-tree");
    let cid = scheduler.register_observe_client(client);
    let program = fe.program;
    let env = fe.env;
    let strand = scheduler.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid) {
            EvalOutcome::Clean(Value::Str(bytes)) if bytes.as_ref() == "nested-tree" => {
                Outcome::Clean
            }
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected nested-tree bytes, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    scheduler.demand(strand);
    scheduler.run();
    assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    drop(producer);
}
