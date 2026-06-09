//! vx-runner - Worker daemon library.
//!
//! Exposes [`RunnerService`] and [`RunnerAcceptor`] for use by the daemon (in-process).

pub(crate) mod exec;
pub(crate) mod output_tree;
pub(crate) mod sandbox;
pub(crate) mod service;

use std::sync::{Arc, Mutex};
use vx_exec_protocol::ObserveDispatcher;
use vx_runtime::remote::ProducingDispatcher;
use vx_services::{RunnerDispatcher, StoreClient};
use vx_vfsd::VfsdService;

use crate::output_tree::CasOutputTreeService;

/// The runner service implementation.
#[derive(Clone)]
pub struct RunnerService {
    store: Option<Arc<StoreClient>>,
    /// The runner's local CAS-VFS — what `exec` mounts actions into and runs against.
    /// `None` disables `exec` (it has no VFS). Held directly (not over RPC): the
    /// runner is the in-process executor, reached over vox but driving its own VFS.
    vfsd: Option<VfsdService>,
    produced_outputs: Arc<Mutex<Option<CasOutputTreeService>>>,
}

impl Default for RunnerService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RunnerService {
    pub fn new(store: Option<Arc<StoreClient>>) -> Self {
        Self {
            store,
            vfsd: None,
            produced_outputs: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the local CAS-VFS `exec` mounts actions into.
    pub fn with_vfsd(mut self, vfsd: VfsdService) -> Self {
        self.vfsd = Some(vfsd);
        self
    }

    pub fn store(&self) -> Option<Arc<StoreClient>> {
        self.store.clone()
    }

    pub(crate) fn vfsd(&self) -> Option<&VfsdService> {
        self.vfsd.as_ref()
    }

    pub(crate) fn install_produced_outputs(&self, outputs: Option<CasOutputTreeService>) {
        *self.produced_outputs.lock().unwrap() = outputs;
    }

    pub(crate) fn produced_outputs(&self) -> Option<CasOutputTreeService> {
        self.produced_outputs.lock().unwrap().clone()
    }

    pub fn handle_producing_connection(
        &self,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), vox::Metadata> {
        match self.produced_outputs() {
            Some(outputs) => {
                connection.handle_with(ProducingDispatcher::new(outputs));
                Ok(())
            }
            None => {
                tracing::warn!("vx-runner: Producing requested before an output tree exists");
                Err(vox::Metadata::default())
            }
        }
    }
}

/// A [`vox::ConnectionAcceptor`] that routes all connections to the [`RunnerService`].
#[derive(Clone)]
pub struct RunnerAcceptor {
    service: RunnerService,
}

impl RunnerAcceptor {
    pub fn new(service: RunnerService) -> Self {
        Self { service }
    }
}

impl vox::ConnectionAcceptor for RunnerAcceptor {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), vox::Metadata> {
        match request.service() {
            "Runner" => {
                connection.handle_with(RunnerDispatcher::new(self.service.clone()));
                Ok(())
            }
            "Observe" => {
                connection.handle_with(ObserveDispatcher::new(self.service.clone()));
                Ok(())
            }
            "Producing" => self.service.handle_producing_connection(connection),
            other => {
                tracing::warn!("vx-runner: unknown service requested: {}", other);
                Err(vox::Metadata::default())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use vx_exec_protocol::{
        ExecCommand, Observe, ObserveClient, ObserveDispatcher, ObserveRequest, Observed, WireValue,
    };
    use vx_ir::item::ItemKind;
    use vx_ir::reachable;
    use vx_runtime::ability::ProviderRegistry;
    use vx_runtime::compile::{FrontEnd, compile_source};
    use vx_runtime::eval::{
        KernelMode, Outcome as EvalOutcome, Thunk, Value, eval_body_with_exec,
        eval_body_with_exec_and_providers, grammar_coord_value, tool_value,
    };
    use vx_runtime::remote::{Part, PartKey, ProducingClient};
    use vx_runtime::strand::{ObserveOutcome, Outcome, ResourceId, Scheduler, dispatch_observe};
    use vx_services::cas_tree::{MerkleTreeFile, build_merkle_tree};
    use vx_services::chunking::{prepare_blob, store_prepared_blob};
    use vx_services::{ExecOutput, StoreClient, StoreDispatcher};

    use super::{RunnerAcceptor, RunnerService};
    use crate::output_tree::CasOutputTreeService;

    fn entry_body(fe: &FrontEnd, name: &str) -> vx_ir::BodyId {
        fe.program
            .arena
            .items
            .iter()
            .find_map(|it| match &it.kind {
                ItemKind::Function(f) if f.name == name => Some(f.body),
                _ => None,
            })
            .expect("entry function")
    }

    async fn observe_client_for(service: RunnerService) -> ObserveClient {
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(RunnerAcceptor::new(service))
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        vox::initiator_on(client_link)
            .establish::<ObserveClient>()
            .await
            .expect("establish runner-backed Observe client")
    }

    async fn producing_client_for(service: RunnerService) -> ProducingClient {
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(RunnerAcceptor::new(service))
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        vox::initiator_on(client_link)
            .establish::<ProducingClient>()
            .await
            .expect("establish runner-backed Producing client")
    }

    #[derive(Clone)]
    struct CountingObserveService {
        inner: RunnerService,
        observes: Arc<AtomicUsize>,
    }

    impl Observe for CountingObserveService {
        async fn observe(&self, request: ObserveRequest) -> Observed {
            self.observes.fetch_add(1, Ordering::SeqCst);
            <RunnerService as Observe>::observe(&self.inner, request).await
        }
    }

    #[derive(Clone)]
    struct CountingRunnerAcceptor {
        service: CountingObserveService,
    }

    impl vox::ConnectionAcceptor for CountingRunnerAcceptor {
        fn accept(
            &self,
            request: &vox::ConnectionRequest,
            connection: vox::PendingConnection,
        ) -> std::result::Result<(), vox::Metadata> {
            match request.service() {
                "Observe" => {
                    connection.handle_with(ObserveDispatcher::new(self.service.clone()));
                    Ok(())
                }
                "Producing" => self.service.inner.handle_producing_connection(connection),
                _ => Err(vox::Metadata::default()),
            }
        }
    }

    async fn counting_observe_client_for(
        service: RunnerService,
    ) -> (ObserveClient, Arc<AtomicUsize>) {
        let observes = Arc::new(AtomicUsize::new(0));
        let counting = CountingObserveService {
            inner: service,
            observes: Arc::clone(&observes),
        };
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(CountingRunnerAcceptor { service: counting })
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        let client = vox::initiator_on(client_link)
            .establish::<ObserveClient>()
            .await
            .expect("establish counting runner-backed Observe client");
        (client, observes)
    }

    type AfterFirstObserve = Arc<Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>>;

    #[derive(Clone)]
    struct AfterFirstObserveService {
        inner: RunnerService,
        observes: Arc<AtomicUsize>,
        after_first: AfterFirstObserve,
    }

    impl Observe for AfterFirstObserveService {
        async fn observe(&self, request: ObserveRequest) -> Observed {
            let prior = self.observes.fetch_add(1, Ordering::SeqCst);
            let observed = <RunnerService as Observe>::observe(&self.inner, request).await;
            if prior == 0
                && let Some(after_first) = self.after_first.lock().unwrap().take()
            {
                after_first();
            }
            observed
        }
    }

    #[derive(Clone)]
    struct AfterFirstRunnerAcceptor {
        service: AfterFirstObserveService,
    }

    impl vox::ConnectionAcceptor for AfterFirstRunnerAcceptor {
        fn accept(
            &self,
            request: &vox::ConnectionRequest,
            connection: vox::PendingConnection,
        ) -> std::result::Result<(), vox::Metadata> {
            match request.service() {
                "Observe" => {
                    connection.handle_with(ObserveDispatcher::new(self.service.clone()));
                    Ok(())
                }
                "Producing" => self.service.inner.handle_producing_connection(connection),
                _ => Err(vox::Metadata::default()),
            }
        }
    }

    async fn observe_client_with_after_first<F>(
        service: RunnerService,
        after_first: F,
    ) -> (ObserveClient, Arc<AtomicUsize>)
    where
        F: FnOnce() + Send + 'static,
    {
        let observes = Arc::new(AtomicUsize::new(0));
        let service = AfterFirstObserveService {
            inner: service,
            observes: Arc::clone(&observes),
            after_first: Arc::new(Mutex::new(Some(Box::new(after_first)))),
        };
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(AfterFirstRunnerAcceptor { service })
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        let client = vox::initiator_on(client_link)
            .establish::<ObserveClient>()
            .await
            .expect("establish mutating runner-backed Observe client");
        (client, observes)
    }

    async fn store_client() -> (tempfile::TempDir, StoreClient) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let store = vx_store::open(temp.path().join("store").to_string_lossy().parse().unwrap())
            .await
            .expect("open store");
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            let acceptor = vox::acceptor_fn(
                move |request: &vox::ConnectionRequest, connection: vox::PendingConnection| {
                    match request.service() {
                        "Store" => {
                            connection.handle_with(StoreDispatcher::new(store.clone()));
                            Ok(())
                        }
                        _ => Err(vox::Metadata::default()),
                    }
                },
            );
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(acceptor)
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        let client = vox::initiator_on(client_link)
            .establish::<StoreClient>()
            .await
            .expect("establish store client");
        (temp, client)
    }

    async fn live_vfs_runner_service() -> (tempfile::TempDir, RunnerService) {
        let (temp, store) = store_client().await;
        let store = Arc::new(store);
        let configured_mount = vx_vfsd::DEFAULT_CONFIGURED_VFS_MOUNT_PATH
            .parse()
            .expect("utf8 VFS mount path");
        let mount = vx_vfsd::effective_mount_path(&configured_mount);
        let vfsd = vx_vfsd::boot_with_store(Arc::clone(&store), &mount)
            .await
            .expect("boot live CAS-VFS");
        (temp, RunnerService::new(Some(store)).with_vfsd(vfsd))
    }

    async fn output_tree(store: &StoreClient) -> vx_services::Blake3Hash {
        let blob = prepare_blob(b"vconn-payload");
        let blob_ref = store_prepared_blob(store, &blob, b"vconn-payload")
            .await
            .expect("store blob");
        let (tree, _) = build_merkle_tree(
            store,
            vec![MerkleTreeFile {
                path: "y.txt".to_string(),
                kind: blob_ref.to_dir_entry_kind(false),
            }],
        )
        .await
        .expect("build output tree");
        tree.0
    }

    fn observer_bundle() -> vx_ir::Bundle {
        let fe = compile_source("fn observe(run: Run) -> Exit ( run.ok )", "observe")
            .expect("front-end compiles");
        let entry = entry_body(&fe, "observe");
        reachable(&fe.program.arena, &fe.env, entry)
    }

    fn find_cc() -> Option<String> {
        ["cc", "clang", "gcc"]
            .into_iter()
            .find_map(vx_runtime::host::which)
    }

    fn cc_provider(binary: &str, version: u64) -> Value {
        Value::Struct(vec![
            (
                "command".to_string(),
                Thunk::done(grammar_coord_value("@vixen/grammar-cc", "cc")),
            ),
            (
                "binary".to_string(),
                Thunk::done(Value::Str(Rc::from(binary))),
            ),
            ("version".to_string(), Thunk::done(Value::U64(version))),
        ])
    }

    fn tool_value_with_fingerprint(
        package: &str,
        command: &str,
        binary: &str,
        fingerprint: &str,
    ) -> Value {
        Value::Struct(vec![
            (
                "command".to_string(),
                Thunk::done(grammar_coord_value(package, command)),
            ),
            (
                "binary".to_string(),
                Thunk::done(Value::Str(Rc::from(binary))),
            ),
            (
                "fingerprint".to_string(),
                Thunk::done(Value::Str(Rc::from(fingerprint))),
            ),
        ])
    }

    #[test]
    fn strand_observe_reaches_runner_backed_service() {
        let scheduler = Scheduler::new(&[1]);
        let client = scheduler.block_on(observe_client_for(RunnerService::default()));
        let client = scheduler.register_observe_client(client);
        let strand = scheduler.spawn(ResourceId(0), move || {
            let observed = dispatch_observe(
                client,
                ObserveRequest {
                    observer: None,
                    command: ExecCommand {
                        program: "true".to_string(),
                        args: Vec::new(),
                    },
                    env: Vec::new(),
                    mounts: Vec::new(),
                    outputs: Vec::new(),
                    create_dirs: Vec::new(),
                },
            );
            match observed.as_ref() {
                ObserveOutcome::Poisoned(reason)
                    if reason.contains("runner-backed observe has no CAS-VFS configured") =>
                {
                    Outcome::Clean
                }
                other => Outcome::Poisoned(format!(
                    "expected runner-backed no-CAS-VFS poison, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn strand_observe_uses_runner_backed_live_vfs_for_success_and_failure() {
        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let client = scheduler.block_on(observe_client_for(service));
        let cid = scheduler.register_observe_client(client);

        let ok = scheduler.spawn(ResourceId(0), move || {
            let observed = dispatch_observe(
                cid,
                ObserveRequest {
                    observer: Some(observer_bundle()),
                    command: ExecCommand {
                        program: "/bin/true".to_string(),
                        args: Vec::new(),
                    },
                    env: Vec::new(),
                    mounts: Vec::new(),
                    outputs: Vec::new(),
                    create_dirs: Vec::new(),
                },
            );
            match observed.as_ref() {
                ObserveOutcome::Value(_) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected /bin/true observe to cross a clean run.ok value, got {other:?}"
                )),
            }
        });
        let failed = scheduler.spawn(ResourceId(0), move || {
            let observed = dispatch_observe(
                cid,
                ObserveRequest {
                    observer: Some(observer_bundle()),
                    command: ExecCommand {
                        program: "/bin/false".to_string(),
                        args: Vec::new(),
                    },
                    env: Vec::new(),
                    mounts: Vec::new(),
                    outputs: Vec::new(),
                    create_dirs: Vec::new(),
                },
            );
            match observed.as_ref() {
                ObserveOutcome::Poisoned(reason) if !reason.is_empty() => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected /bin/false observe to cross run.ok poison, got {other:?}"
                )),
            }
        });
        scheduler.demand(ok);
        scheduler.demand(failed);
        scheduler.run();
        assert_eq!(scheduler.outcome(ok), Some(Outcome::Clean));
        assert_eq!(scheduler.outcome(failed), Some(Outcome::Clean));
    }

    #[test]
    fn demanded_vixen_exec_reaches_runner_backed_observe() {
        let fe = compile_source(
            "fn t(cmd: Command) -> Exit ( `$cmd` with { observer: (run) => run.ok } )",
            "t",
        )
        .expect("exec program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let client = scheduler.block_on(observe_client_for(RunnerService::default()));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "true");
            match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid)
            {
                EvalOutcome::Poisoned { reason, .. }
                    if reason.contains("runner-backed observe has no CAS-VFS configured") =>
                {
                    Outcome::Clean
                }
                other => Outcome::Poisoned(format!(
                    "expected demanded exec to reach runner no-CAS-VFS poison, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    fn undemanded_vixen_exec_with_runner_backed_client_stays_cold() {
        let fe = compile_source(
            "fn t(cmd: Command) -> U64 ( \
               let pair = ( `$cmd` with { observer: (run) => run.ok }, 42 ); \
               pair[1] )",
            "t",
        )
        .expect("exec demand program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let client = scheduler.block_on(observe_client_for(RunnerService::default()));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "false");
            match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid)
            {
                EvalOutcome::Clean(Value::U64(42)) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "undemanded exec should stay cold and return 42, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn run_ok_observer_crosses_runner_backed_live_vfs_for_success_and_failure() {
        let fe = compile_source(
            "fn t(cmd: Command) -> Exit ( `$cmd` with { observer: (run) => run.ok } )",
            "t",
        )
        .expect("exec run.ok program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let client = scheduler.block_on(observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let ok = tool_value("@vixen/grammar-cc", "cc", "/bin/true");
            match eval_body_with_exec(&program, &env, entry, vec![ok], KernelMode::GraphOnly, cid) {
                EvalOutcome::Clean(Value::Struct(fields)) => {
                    let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
                    if names != ["stdout", "stderr"] {
                        return Outcome::Poisoned(format!(
                            "run.ok returned struct fields {names:?}, expected stdout/stderr"
                        ));
                    }
                }
                other => {
                    return Outcome::Poisoned(format!(
                        "expected /bin/true run.ok to return Exit over runner-backed live CAS-VFS, \
                         got {other:?}"
                    ));
                }
            }

            let failed = tool_value("@vixen/grammar-cc", "cc", "/bin/false");
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![failed],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Poisoned { reason, .. } if !reason.is_empty() => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected /bin/false run.ok to poison over runner-backed live CAS-VFS, \
                     got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn identical_backtick_exec_boundaries_share_runner_backed_l1_entry() {
        let fe = compile_source(
            "fn t(cmd: Command, script: String) ( \
               let first = `$cmd -c ${script}` with { writable: [\"out\"] }; \
               let second = `$cmd -c ${script}` with { writable: [\"out\"] }; \
               assert_eq(first.file(\"out/y.txt\"), second.file(\"out/y.txt\")) )",
            "t",
        )
        .expect("exec cache program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let (client, observes) = scheduler.block_on(counting_observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "/bin/sh");
            let script = Value::Str(Rc::from("printf runner-cache > out/y.txt"));
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![cmd, script],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected identical runner-backed execs to share output, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
        assert_eq!(
            observes.load(Ordering::SeqCst),
            1,
            "the second identical backtick exec must reuse the L1 observe result"
        );
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn writable_roots_disambiguate_runner_backed_l1_entry() {
        let fe = compile_source(
            "fn t(cmd: Command, script: String) ( \
               let first = `$cmd -c ${script}` with { writable: [\"out-a\"] }; \
               let second = `$cmd -c ${script}` with { writable: [\"out-b\"] }; \
               assert((first.file(\"out-a/y.txt\") == \"runner-a\") && \
                      (second.file(\"out-b/y.txt\") == \"runner-b\")) )",
            "t",
        )
        .expect("exec writable-cache program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let (client, observes) = scheduler.block_on(counting_observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "/bin/sh");
            let script = Value::Str(Rc::from(
                "if [ -d out-a ]; then printf runner-a > out-a/y.txt; else printf runner-b > out-b/y.txt; fi",
            ));
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![cmd, script],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected writable-root-distinct runner-backed execs to resolve, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
        assert_eq!(
            observes.load(Ordering::SeqCst),
            2,
            "different writable roots must not alias in the runner-backed L1"
        );
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn observer_identity_disambiguates_runner_backed_l1_entry() {
        let fe = compile_source(
            "fn id(exit: Exit) -> Exit ( exit ) \
             fn t(cmd: Command, script: String) ( \
               let direct = `$cmd -c ${script}` with { observer: (run) => run.ok }; \
               let wrapped = `$cmd -c ${script}` with { observer: (run) => id(run.ok) }; \
               assert_eq(direct.stdout, wrapped.stdout) )",
            "t",
        )
        .expect("exec observer-cache program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let (client, observes) = scheduler.block_on(counting_observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "/bin/sh");
            let script = Value::Str(Rc::from("printf runner-observer"));
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![cmd, script],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected observer-distinct runner-backed execs to resolve, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
        assert_eq!(
            observes.load(Ordering::SeqCst),
            2,
            "different observer bundles must not alias in the runner-backed L1"
        );
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn declared_tool_fingerprint_disambiguates_runner_backed_l1_entry() {
        let fe = compile_source(
            "fn t(first_cmd: Command, second_cmd: Command, script: String) ( \
               let first = `$first_cmd -c ${script}` with { observer: (run) => run.ok }; \
               let second = `$second_cmd -c ${script}` with { observer: (run) => run.ok }; \
               assert_eq(first.stdout, second.stdout) )",
            "t",
        )
        .expect("exec declared-fingerprint program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let (client, observes) = scheduler.block_on(counting_observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let first =
                tool_value_with_fingerprint("@vixen/grammar-cc", "cc", "/bin/sh", "declared:first");
            let second = tool_value_with_fingerprint(
                "@vixen/grammar-cc",
                "cc",
                "/bin/sh",
                "declared:second",
            );
            let script = Value::Str(Rc::from("printf runner-declared"));
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![first, second, script],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected declared-fingerprint-distinct runner-backed execs to resolve, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
        assert_eq!(
            observes.load(Ordering::SeqCst),
            2,
            "different declared tool fingerprints must not alias in the runner-backed L1"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn executable_fingerprint_disambiguates_runner_backed_l1_entry() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new().expect("executable fingerprint temp dir");
        let link = temp.path().join("tool");
        let first_path = temp.path().join("tool.first");
        let second_path = temp.path().join("tool.second");
        std::fs::write(&first_path, "#!/bin/sh\nprintf same\n# first\n")
            .expect("write first executable");
        std::fs::write(&second_path, "#!/bin/sh\nprintf same\n# second\n")
            .expect("write second executable");
        for path in [&first_path, &second_path] {
            let mut perms = std::fs::metadata(path)
                .expect("executable metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).expect("make executable runnable");
        }
        std::os::unix::fs::symlink(&first_path, &link).expect("link executable path to v1");

        let fe = compile_source(
            "fn t(cmd: Command) ( \
               let first = `$cmd` with { observer: (run) => run.ok }; \
               let second = `$cmd` with { observer: (run) => run.ok }; \
               assert_eq(first.stdout, second.stdout) )",
            "t",
        )
        .expect("exec binary-fingerprint program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let link_for_hook = link.clone();
        let second_for_hook = second_path.clone();
        let (client, observes) =
            scheduler.block_on(observe_client_with_after_first(service, move || {
                std::fs::remove_file(&link_for_hook).expect("remove v1 executable link");
                std::os::unix::fs::symlink(&second_for_hook, &link_for_hook)
                    .expect("relink executable path to v2");
            }));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let binary = link.to_string_lossy().into_owned();
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", &binary);
            match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid) {
                EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected executable-fingerprint-distinct runner-backed execs to resolve, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
        assert_eq!(
            observes.load(Ordering::SeqCst),
            2,
            "changing executable content at one path must not alias in the runner-backed L1"
        );
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn demanded_vixen_exec_runs_through_runner_backed_live_vfs_and_serves_output_tree() {
        let fe = compile_source(
            "fn t(cmd: Command, script: String) -> Blob ( \
               let out = `$cmd -c ${script}` with { writable: [\"out\"] }; \
               out.file(\"out/y.txt\") )",
            "t",
        )
        .expect("exec output-tree program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let client = scheduler.block_on(observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "/bin/sh");
            let script = Value::Str(Rc::from("printf runner-vfs-payload > out/y.txt"));
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![cmd, script],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Clean(Value::Str(bytes)) if bytes.as_ref() == "runner-vfs-payload" => {
                    Outcome::Clean
                }
                other => Outcome::Poisoned(format!(
                    "expected demanded .vx exec to run through runner-backed live CAS-VFS \
                     and serve out/y.txt, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn explicit_stream_observer_returns_runner_backed_live_output_tree() {
        let fe = compile_source(
            "fn t(cmd: Command, script: String) -> Blob ( \
               let out = `$cmd -c ${script}` with { \
                   writable: [\"out\"], \
                   observer: (run) => run.stream.out \
               }; \
               out.file(\"out/y.txt\") )",
            "t",
        )
        .expect("exec stream-output program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let client = scheduler.block_on(observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "/bin/sh");
            let script = Value::Str(Rc::from("printf stream-vfs-payload > out/y.txt"));
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![cmd, script],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Clean(Value::Str(bytes)) if bytes.as_ref() == "stream-vfs-payload" => {
                    Outcome::Clean
                }
                other => Outcome::Poisoned(format!(
                    "expected explicit run.stream.out observer to serve out/y.txt through \
                     runner-backed live CAS-VFS, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn bare_invocation_failure_poisons_through_runner_backed_live_vfs() {
        let fe = compile_source(
            "fn t(cmd: Command, script: String) -> Blob ( \
               let out = `$cmd -c ${script}` with { writable: [\"out\"] }; \
               out.file(\"out/y.txt\") )",
            "t",
        )
        .expect("exec failure program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let client = scheduler.block_on(observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "/bin/sh");
            let script = Value::Str(Rc::from("exit 7"));
            match eval_body_with_exec(
                &program,
                &env,
                entry,
                vec![cmd, script],
                KernelMode::GraphOnly,
                cid,
            ) {
                EvalOutcome::Poisoned { reason, .. } if !reason.is_empty() => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected bare invocation run.ok.out gate to poison through runner-backed \
                     live CAS-VFS, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount and a host C compiler"]
    fn hello_world_uses_runner_backed_live_vfs_and_where_injection() {
        let cc = find_cc().expect("live VFS hello-world proof needs a host C compiler");
        let dir = tempfile::TempDir::new().expect("hello source temp dir");
        let hello = dir.path().join("hello.c");
        std::fs::write(&hello, "int main(void) { return 0; }\n").expect("write hello.c");
        let hello_path = hello.to_str().expect("utf-8 path").to_string();

        let fe = compile_source(
            "struct Cc { version: U64 }\n\
             fn t(hello: String) -> Blob ( \
               let cc = Cc where (version >= 1); \
               let obj = `$cc -c ${hello} -o out/hello.o` with { writable: [\"out\"] }; \
               obj.file(\"out/hello.o\") )",
            "t",
        )
        .expect("hello-world program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_temp, service) = scheduler.block_on(live_vfs_runner_service());
        let client = scheduler.block_on(observe_client_for(service));
        let cid = scheduler.register_observe_client(client);
        let program = fe.program;
        let env = fe.env;
        let strand = scheduler.spawn(ResourceId(0), move || {
            let mut providers = ProviderRegistry::new();
            providers.advertise("Cc", cc_provider(&cc, 1));
            let hello = Value::Str(Rc::from(hello_path.as_str()));
            match eval_body_with_exec_and_providers(
                &program,
                &env,
                entry,
                vec![hello],
                KernelMode::GraphOnly,
                cid,
                providers,
            ) {
                EvalOutcome::Clean(Value::Str(bytes)) if !bytes.is_empty() => Outcome::Clean,
                EvalOutcome::Clean(other) => {
                    Outcome::Poisoned(format!("expected non-empty object bytes, got {other:?}"))
                }
                EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }

    #[test]
    fn producing_vconn_serves_runner_output_tree_from_cas() {
        let scheduler = Scheduler::new(&[1]);
        let (_temp, store) = scheduler.block_on(store_client());
        let tree = scheduler.block_on(output_tree(&store));
        let service = RunnerService::new(Some(Arc::new(store)));
        service.install_produced_outputs(Some(CasOutputTreeService::new(
            service.store().expect("store"),
            vec![ExecOutput {
                path: "out".to_string(),
                tree,
            }],
        )));
        let producing = scheduler.block_on(producing_client_for(service));
        let got = scheduler
            .block_on(producing.force(PartKey::TreePath("out/y.txt".to_string())))
            .expect("force output path");
        assert_eq!(got, Part::Bytes(b"vconn-payload".to_vec()));
    }

    #[test]
    fn value_with_tree_observe_installs_runner_output_tree_from_cas() {
        let scheduler = Scheduler::new(&[1]);
        let (_temp, store) = scheduler.block_on(store_client());
        let tree = scheduler.block_on(output_tree(&store));
        let service = RunnerService::new(Some(Arc::new(store)));
        let observed = service.install_producing_outputs_for_observed(
            Observed::ValueWithTree(WireValue::Struct(vec![(
                "out".to_string(),
                WireValue::Tree,
            )])),
            vec![ExecOutput {
                path: "out".to_string(),
                tree,
            }],
        );
        assert!(
            matches!(observed, Observed::ValueWithTree(value) if value.contains_tree()),
            "tree-bearing observer values should cross unchanged after installing Producing"
        );
        let producing = scheduler.block_on(producing_client_for(service));
        let got = scheduler
            .block_on(producing.force(PartKey::TreePath("out/y.txt".to_string())))
            .expect("force output path");
        assert_eq!(got, Part::Bytes(b"vconn-payload".to_vec()));
    }
}
