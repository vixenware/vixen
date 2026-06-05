use vx_runtime::cache::{RuntimeCacheFuture, RuntimeCacheRecordKey, RuntimeCacheService};
use vx_services::{
    Blake3Hash, CacheIndexClient, CacheIndexKey, CacheInputKeyDigest, CacheLookupRequest,
    CachePutRequest, CacheRecordBody, CacheRecordNode, CasNode, StoreClient,
};

const RUNTIME_CACHE_RECORD_FORMAT: &str = "vx-runtime-cache-record-json-v1";

#[derive(Clone)]
pub struct CacheIndexRuntimeCacheService {
    cache_index: CacheIndexClient,
    store: StoreClient,
    lookup_limit: u32,
}

impl CacheIndexRuntimeCacheService {
    pub fn new(cache_index: CacheIndexClient, store: StoreClient) -> Self {
        Self {
            cache_index,
            store,
            lookup_limit: 16,
        }
    }
}

impl RuntimeCacheService for CacheIndexRuntimeCacheService {
    fn get_record<'a>(
        &'a self,
        key: RuntimeCacheRecordKey,
    ) -> RuntimeCacheFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move { self.get_record_inner(key).await })
    }

    fn put_record<'a>(
        &'a self,
        key: RuntimeCacheRecordKey,
        record: String,
    ) -> RuntimeCacheFuture<'a, Result<(), String>> {
        Box::pin(async move { self.put_record_inner(key, record).await })
    }
}

impl CacheIndexRuntimeCacheService {
    async fn get_record_inner(&self, key: RuntimeCacheRecordKey) -> Result<Option<String>, String> {
        let input_key = input_key_digest(&key)?;
        let lookup = self
            .cache_index
            .lookup_cache_records(CacheLookupRequest {
                key: cache_index_key(&key, input_key),
                limit: self.lookup_limit,
            })
            .await
            .map_err(|error| format!("cache-index lookup failed: {error:?}"))?;
        for record_hash in lookup.records {
            let Some(bytes) = self
                .store
                .get_node(record_hash)
                .await
                .map_err(|error| format!("cache record fetch failed: {error:?}"))?
            else {
                continue;
            };
            let node = CasNode::from_bytes(&bytes)
                .map_err(|error| format!("cache record {record_hash} is invalid: {error}"))?;
            let CasNode::CacheRecord(record) = node else {
                continue;
            };
            if record.namespace != key.namespace() || record.input_key != input_key {
                continue;
            }
            let CacheRecordBody::Opaque { format, bytes } = record.body else {
                continue;
            };
            if format != RUNTIME_CACHE_RECORD_FORMAT {
                continue;
            }
            let text = String::from_utf8(bytes)
                .map_err(|error| format!("cache record {record_hash} is not UTF-8: {error}"))?;
            return Ok(Some(text));
        }
        Ok(None)
    }

    async fn put_record_inner(
        &self,
        key: RuntimeCacheRecordKey,
        record: String,
    ) -> Result<(), String> {
        let input_key = input_key_digest(&key)?;
        let node = CasNode::CacheRecord(CacheRecordNode {
            namespace: key.namespace().to_string(),
            input_key,
            body: CacheRecordBody::Opaque {
                format: RUNTIME_CACHE_RECORD_FORMAT.to_string(),
                bytes: record.into_bytes(),
            },
            read_witness: None,
        });
        let (bytes, record_hash) = node.to_bytes_and_hash();
        self.store
            .put_node(record_hash, bytes)
            .await
            .map_err(|error| format!("cache record store failed: {error:?}"))?;
        self.cache_index
            .put_cache_record(CachePutRequest {
                key: cache_index_key(&key, input_key),
                record: record_hash,
            })
            .await
            .map_err(|error| format!("cache-index put failed: {error:?}"))?;
        Ok(())
    }
}

fn input_key_digest(key: &RuntimeCacheRecordKey) -> Result<CacheInputKeyDigest, String> {
    Blake3Hash::from_hex(key.input_digest())
        .map(CacheInputKeyDigest)
        .ok_or_else(|| {
            format!(
                "runtime cache input digest `{}` is not a blake3 hex digest",
                key.input_digest()
            )
        })
}

fn cache_index_key(key: &RuntimeCacheRecordKey, input_key: CacheInputKeyDigest) -> CacheIndexKey {
    CacheIndexKey {
        namespace: key.namespace().to_string(),
        input_key,
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use camino::Utf8PathBuf;
    use vx_exec_protocol::{Observe, ObserveClient, ObserveDispatcher, ObserveRequest, Observed};
    use vx_ir::item::ItemKind;
    use vx_runner::RunnerService;
    use vx_runtime::cache::StrandCacheBackend;
    use vx_runtime::compile::{FrontEnd, compile_source, semantic_front_end_bundled};
    use vx_runtime::eval::{
        KernelMode, Outcome as EvalOutcome, Value, eval_body_with_cache_backend,
        eval_body_with_exec_and_cache_backend, tool_value,
    };
    use vx_runtime::strand::{Outcome, ResourceId, Scheduler};
    use vx_services::{
        CacheIndex, CacheIndexClient, CacheIndexDispatcher, CacheLookupRequest, CacheLookupResult,
        CachePutRequest, StoreClient,
    };

    use crate::{CacheIndexRuntimeCacheService, CacheIndexService};

    #[derive(Clone, Default)]
    struct CountingCacheIndex {
        inner: CacheIndexService,
        puts: Arc<AtomicUsize>,
    }

    impl CountingCacheIndex {
        fn put_count(&self) -> usize {
            self.puts.load(Ordering::SeqCst)
        }
    }

    impl CacheIndex for CountingCacheIndex {
        async fn lookup_cache_records(&self, request: CacheLookupRequest) -> CacheLookupResult {
            self.inner.lookup_cache_records(request).await
        }

        async fn put_cache_record(&self, request: CachePutRequest) -> Result<(), String> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.inner.put_cache_record(request).await
        }
    }

    async fn cache_index_client_for(service: CountingCacheIndex) -> CacheIndexClient {
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            let acceptor = vox::acceptor_fn(
                move |request: &vox::ConnectionRequest, connection: vox::PendingConnection| {
                    match request.service() {
                        "CacheIndex" => {
                            connection.handle_with(CacheIndexDispatcher::new(service.clone()));
                            Ok(())
                        }
                        _ => Err(vec![]),
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
        vox::initiator_on(client_link, vox::TransportMode::Bare)
            .establish::<CacheIndexClient>()
            .await
            .expect("establish cache-index client")
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
        ) -> std::result::Result<(), vox::Metadata<'static>> {
            match request.service() {
                "Observe" => {
                    connection.handle_with(ObserveDispatcher::new(self.service.clone()));
                    Ok(())
                }
                "Producing" => self.service.inner.handle_producing_connection(connection),
                _ => Err(vec![]),
            }
        }
    }

    async fn counting_observe_client_for(
        service: RunnerService,
    ) -> (ObserveClient, Arc<AtomicUsize>) {
        let observes = Arc::new(AtomicUsize::new(0));
        let service = CountingObserveService {
            inner: service,
            observes: Arc::clone(&observes),
        };
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(CountingRunnerAcceptor { service })
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        let client = vox::initiator_on(client_link, vox::TransportMode::Bare)
            .establish::<ObserveClient>()
            .await
            .expect("establish counting runner-backed Observe client");
        (client, observes)
    }

    async fn live_vfs_runner_service() -> (tempfile::TempDir, StoreClient, RunnerService) {
        let temp = tempfile::tempdir().expect("temp dir");
        let store_root =
            Utf8PathBuf::from_path_buf(temp.path().join("store")).expect("utf8 store path");
        let store_service = vx_store::open(store_root).await.expect("open store");
        let store = crate::connect_in_process_store(store_service)
            .await
            .expect("connect store");
        let store = Arc::new(store);
        let mount = "/tmp/vx/vfs".parse().expect("utf8 VFS mount path");
        let vfsd = vx_vfsd::boot_with_store(Arc::clone(&store), &mount)
            .await
            .expect("boot live CAS-VFS");
        (
            temp,
            store.as_ref().clone(),
            RunnerService::new(Some(store)).with_vfsd(vfsd),
        )
    }

    fn body_in(program: &vx_ir::build::VirProgram, entry: &str) -> vx_ir::BodyId {
        let primary = *program.source_units.first().expect("source unit");
        program.arena[primary]
            .items
            .iter()
            .find_map(|&id| match &program.arena[id].kind {
                ItemKind::Function(function) if function.name == entry => Some(function.body),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fn {entry}"))
    }

    fn entry_body(fe: &FrontEnd, name: &str) -> vx_ir::BodyId {
        fe.program
            .arena
            .items
            .iter()
            .find_map(|item| match &item.kind {
                ItemKind::Function(function) if function.name == name => Some(function.body),
                _ => None,
            })
            .expect("entry function")
    }

    #[test]
    fn cache_index_store_backend_reuses_function_output_through_strand_parking() {
        let fe = semantic_front_end_bundled(
            "fn answer() -> U64 ( 40 + 2 ) fn main() -> U64 ( answer() )",
        )
        .expect("front-end");
        let body = body_in(&fe.program, "main");

        let scheduler = Scheduler::new(&[1]);
        let temp = tempfile::tempdir().expect("temp dir");
        let store_root =
            Utf8PathBuf::from_path_buf(temp.path().join("store")).expect("utf8 store path");
        let store_service = scheduler
            .block_on(vx_store::open(store_root))
            .expect("open store");
        let store = scheduler
            .block_on(crate::connect_in_process_store(store_service))
            .expect("connect store");
        let cache_index = CountingCacheIndex::default();
        let cache_index_client = scheduler.block_on(cache_index_client_for(cache_index.clone()));
        let cache_service = CacheIndexRuntimeCacheService::new(cache_index_client, store);
        let cache_client = scheduler.register_cache_service(cache_service);
        let puts = cache_index.puts.clone();
        let program = fe.program;
        let env = fe.env;

        let strand = scheduler.spawn(ResourceId(0), move || {
            let backend = StrandCacheBackend::new(cache_client);
            let first = eval_body_with_cache_backend(
                &program,
                &env,
                body,
                vec![],
                KernelMode::Off,
                backend,
            );
            if !matches!(first, EvalOutcome::Clean(Value::U64(42))) {
                return Outcome::Poisoned(format!("first eval got {first:?}"));
            }
            let puts_after_first = puts.load(Ordering::SeqCst);
            if puts_after_first == 0 {
                return Outcome::Poisoned(
                    "first eval should write a cache record through CacheIndex".to_string(),
                );
            }

            let second = eval_body_with_cache_backend(
                &program,
                &env,
                body,
                vec![],
                KernelMode::Off,
                StrandCacheBackend::new(cache_client),
            );
            if !matches!(second, EvalOutcome::Clean(Value::U64(42))) {
                return Outcome::Poisoned(format!("second eval got {second:?}"));
            }
            let puts_after_second = puts.load(Ordering::SeqCst);
            if puts_after_second != puts_after_first {
                return Outcome::Poisoned(format!(
                    "second eval should reuse the service-backed record; puts went from \
                     {puts_after_first} to {puts_after_second}"
                ));
            }
            Outcome::Clean
        });
        scheduler.demand(strand);
        scheduler.run();

        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
        assert_eq!(cache_index.put_count(), 1);
    }

    #[test]
    #[ignore = "needs the live CAS-VFS mount; run with --ignored on a host with the Vixen VFS"]
    fn backtick_exec_cache_index_store_backend_survives_fresh_runner_backed_graph_eval() {
        let fe = compile_source(
            "fn t(cmd: Command, script: String) ( \
               let run = `$cmd -c ${script}` with { observer: (run) => run.ok }; \
               assert_eq(run.stdout, run.stdout) )",
            "t",
        )
        .expect("exec l2 cache program compiles");
        let entry = entry_body(&fe, "t");

        let scheduler = Scheduler::new(&[1]);
        let (_vfs_temp, store, service) = scheduler.block_on(live_vfs_runner_service());
        let (observe_client, observes) = scheduler.block_on(counting_observe_client_for(service));
        let observe_client = scheduler.register_observe_client(observe_client);
        let cache_index = CountingCacheIndex::default();
        let cache_index_client = scheduler.block_on(cache_index_client_for(cache_index.clone()));
        let cache_service = CacheIndexRuntimeCacheService::new(cache_index_client, store);
        let cache_client = scheduler.register_cache_service(cache_service);
        let cache_puts = cache_index.puts.clone();
        let program = fe.program;
        let env = fe.env;

        let strand = scheduler.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", "/bin/sh");
            let script = Value::Str(Rc::from("printf runner-l2"));
            let first = eval_body_with_exec_and_cache_backend(
                &program,
                &env,
                entry,
                vec![cmd.clone(), script.clone()],
                KernelMode::Off,
                observe_client,
                StrandCacheBackend::new(cache_client),
            );
            if !matches!(first, EvalOutcome::Clean(Value::Unit)) {
                return Outcome::Poisoned(format!(
                    "expected first runner-backed cache-index eval to return unit, got {first:?}"
                ));
            }
            let cache_puts_after_first = cache_puts.load(Ordering::SeqCst);
            if cache_puts_after_first == 0 {
                return Outcome::Poisoned(
                    "first runner-backed eval should write a CacheIndex record".to_string(),
                );
            }

            match eval_body_with_exec_and_cache_backend(
                &program,
                &env,
                entry,
                vec![cmd, script],
                KernelMode::Off,
                observe_client,
                StrandCacheBackend::new(cache_client),
            ) {
                EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
                other => Outcome::Poisoned(format!(
                    "expected cached runner-backed cache-index eval to return unit, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();

        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
        assert_eq!(
            observes.load(Ordering::SeqCst),
            1,
            "fresh graph eval with the same CacheIndex/Store backend must reuse the \
             persisted observe value"
        );
        assert_eq!(
            cache_index.put_count(),
            1,
            "the second graph eval must not write a duplicate cache-index record"
        );
    }
}
