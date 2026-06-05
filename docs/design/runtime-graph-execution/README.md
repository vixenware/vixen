# Runtime graph execution roadmap

This document is the self-contained roadmap for replacing `vx-runtime`'s
expression tree-walker with a graph-driven runtime. It exists because chat
context is not a reliable control surface for a multi-commit runtime rebuild.

Every document in this directory repeats the core task context on purpose. Future
agents must be able to open any one file after compaction and recover the same
constraints: the runtime must execute graph nodes, not source expressions; the
execution crates must not depend on VIR; unsupported graph shapes must fail
explicitly instead of falling back to source interpretation.

## Complete task context

Vixen's intended runtime is a lazy demand sea with eager pure kernel islands. The
CLI or external caller creates exactly one root demand: the entry point's return
value. Evaluation flows backward through a demand graph. Values in the
orchestrator are references to graph nodes; "resolved" is a node memo fact. A
lazy-data constructor references its fields without demanding them. A boundary
node such as `exec` or `http` runs only when demand reaches it. Pure computation
collapses into eager kernels.

The architectural failure mode is an eager recursive expression interpreter. It
looks at `ExprKind`, evaluates subexpressions directly, and can force values at
internal positions because the Rust call stack makes that easy. A graph wrapper
around such an interpreter is not enough. If a node executor can map a node back
to an `ExprId` and call `eval_inner`, the old shape has survived under a new
name.

This exact failure has already happened. Commit `3fed572f` added graph frames and
per-node memoization around the existing tree-walker, but `force_node` still
looked up source expressions and called `eval`. That is scaffolding, not the
engine. Commit `4c55d433` created the first structural wall:

- `vx-runtime-graph`: graph types, source-blind.
- `vx-runtime-core`: graph stepper, source-blind, no scheduling or demand-origin
  constructors.
- `vx-runtime-driver`: external demand requests, memo table, and work loop.
- `vx-runtime::graph`: compatibility re-export of `vx-runtime-graph`.

The old `vx-runtime::eval` tree-walker has been deleted from compiled runtime
code. The next implementation work is to keep collapsing the temporary public
`Value`/`Thunk` edge into graph node references and then implement the specified
input-keyed cache. Green tests produced by expression interpretation are still
invalid; there should be no expression interpreter left to produce them.

## Non-negotiables

- Analyzer and lowering code may traverse VIR. Execution code may not.
- Execution crates must not import or mention `vx_ir`, `ExprKind`, `StmtKind`,
  `PatternKind`, `ExprId`, `PatternId`, `VirProgram`, or `BodyId`.
- Runtime execution must not expose a generic internal `force(node) -> value`
  operation. The driver owns external demand origins and scheduling.
- Core execution returns `Need`, `Ready`, `Boundary`, `Poison`, or `Unsupported`.
  It does not satisfy its own dependencies.
- Public runtime entry points must enter through graph driver APIs.
- Unsupported graph shapes fail explicitly as graph unsupported errors. They do
  not fall back to source evaluation.
- `KernelMode` variants must be graph-kernel policy labels, never tree-walker
  oracle labels. `GraphOnly` is graph execution without an additional native
  kernel preference, not a ground-truth interpreter.
- The bomb detonates only when the graph driver demands its node.
- Cache keys are input-keyed and computed before running; read-sets are
  post-run provenance, never key material.

## Current checkpoint to verify

Do not trust this paragraph as status; verify it in code before continuing.

Expected latest structural checkpoint:

- Commit `4c55d433` exists: `Fence graph execution away from VIR`.
- Commit `bfcc1493` exists: `Document graph runtime demolition roadmap`.
- `Cargo.toml` includes `crates/vx-runtime-graph`,
  `crates/vx-runtime-core`, `crates/vx-runtime-driver`,
  `crates/vx-exec-protocol`, and `crates/vx-cache-index`.
- `crates/vx-runtime-core/Cargo.toml` has no `vx-ir` dependency.
- `crates/vx-runtime-driver/Cargo.toml` has no `vx-ir` dependency.
- `crates/vx-runtime/src/graph.rs` only re-exports `vx_runtime_graph`.
- `crates/vx-runtime/tests/source_blind_guardrails.rs` scans
  `vx-runtime-core` and `vx-runtime-driver` source/manifests for forbidden
  source-evaluator affordances, scans `vx-runtime/src` for the deleted runtime
  tree-walker affordances, and guards against reintroducing runtime-local
  process-spawning observe code or runtime-owned exec/observe protocol
  declarations.
- `docs/design/runtime-graph-execution/07-exec-observe-morph.md` records the
  active plan for the runner-backed exec path and the remaining cache/index
  ownership work.
- `vx-runtime::eval::{run, run_with_dispatches, run_in_mode}` enter through
  `graph_exec::run_source` and `vx-runtime-driver`.
- `vx-runtime-graph::NodeKind::Const` carries `ConstValue`; core execution and
  executable lowering no longer parse constant labels for semantics.
- Statically resolved direct function calls expand into graph nodes at analysis
  time with argument node refs bound to callee parameter patterns.
- Closure literals and named functions used as values produce source-blind graph
  `Closure` nodes and known closure/function-value calls expand into graph nodes
  at analysis time.
- `String.len()` executes through source-blind graph `KernelOp::StrLen`.
- Literal/wildcard/binding `match` executes through source-blind
  `NodeKind::Match` payloads.
- Variant construction carries tag and field-name payloads. Core can construct
  variant outputs, project payload fields, and select match arms by variant tag.
- `expr?` executes as graph-level catch/reify through `NodeKind::Fallible` and a
  paired `NodeKind::CaughtFailure`. The driver catches poison from the inner
  demand and memoizes `Failure { err, context }` as graph output.
- Direct and known closure calls leave source-blind `NodeKind::Call` wrappers
  around the expanded body root. Core demands only the body-root input; argument
  edges are retained for graph identity and future call-memo keys without making
  the call boundary strict.
- Direct and closure call nodes now carry a code identity and share a per-run
  driver call memo keyed by function entry, code identity, and already-resolved
  argument/capture outputs before the body runs. Unresolved arguments or
  captures are not demanded to build the key.
- Source-derived direct and closure code identities are now content-sensitive
  hashes (`blake3:<hash>` for functions, `closure:blake3:<hash>` for closures)
  computed from structured VIR body material plus transitive identities for
  referenced function items. Mutually recursive functions share one SCC
  fingerprint; the `Call` node's `function` field disambiguates the entry point
  for memo keys.
- The temporary cache-index L2 can persist self-contained function call outputs
  (`U64`, `Bool`, `String`, `Unit`) under that same call key when an explicit
  cache directory is configured. Outputs containing graph-local node refs remain
  per-run only. The ambient `VX_RUNTIME_L2_DIR` hook has been removed from
  default graph execution.
- `vx-runtime-driver` reports graph kernel executions for the transitional
  public dispatch counter.
- Graph root demand is constructed from the driver's graph root via
  `Driver::root_request()`, not from a public `DemandRequest::root(node)`
  constructor.
- API-edge field demand is constructed from `PublicValueDemand` tokens minted by
  the resolved-node handle returned from the driver, not from a public
  `DemandRequest::public_value_field(node)` constructor.
- `graph_exec` can expose graph-root lazy arrays/tuples/variants and caught
  failures as old public `Value` thunks whose fields carry authorized
  `PublicValueField` demand tokens.
- `eval_body`, `eval_body_with_exec`, `eval_body_with_exec_and_providers`, and
  `eval_body_wire` enter graph body execution. `eval_body_with_exec` passes the
  observe client into graph boundary materialization. External body arguments are
  seeded as graph outputs with external boundary field nodes.
- `host.which` and `host.exec` lower to typed graph host-method boundaries and
  materialize through the graph driver.
- Backtick command invocations lower to graph boundary nodes plus runtime-local
  typed metadata for CLI argument templates and observer bundles. Demand on the
  exec boundary materializes only the request-shaping payload nodes, dispatches
  observe on miss, seeds cached graph output on an exact in-process L1 or opt-in
  disk L2 hit keyed by the materialized request plus the pre-run executable
  content fingerprint, and memoizes the graph output.
- Invocation `env: %{ ... }`, `mounts: %{ ... }`, `writable: [...]`, and `create_dirs: [...]`
  supplements lower into exec boundary metadata as graph node refs, are demanded
  only during boundary materialization, cross in `ObserveRequest.env`,
  `ObserveRequest.mounts`, `ObserveRequest.outputs`, and
  `ObserveRequest.create_dirs`, are included in the exec cache key, and are
  consumed by `vx-runner` as `ExecRequest.env`, `ExecRequest.mounts`,
  `ExecRequest.outputs`, and `ExecRequest.create_dirs`. The runner rejects
  `create_dirs` outside declared writable roots and creates valid nested dirs
  before launch. Env support currently materializes string key/value pairs; the
  full `ExecInput` env surface remains future protocol work. Mount support
  currently materializes resolved content-addressed Tree hashes; remote
  producing output-tree handles are not valid exec input mounts.
- Observer execution uses graph body execution and graph/native wire crossing.
- `crates/vx-exec-protocol` owns `ExecCommand`, `ObserveRequest`,
  `WireValue`, `Observed`, and the generated Observe Vox client/dispatcher.
  `vx-runtime::observe` owns only `CompletedRun` and `evaluate_observer`.
- `vx-runtime` no longer depends on `vx-cache-index` and exposes no cache-dir
  runtime entry point. Cache proofs install a caller-owned
  `RuntimeCacheBackend`; daemon/service-backed proofs install
  `StrandCacheBackend`. `vx-runtime::graph_exec` computes pre-run boundary cache
  keys while materializing demanded boundaries, but it no longer owns the L1
  maps, `LocalCacheIndex`, file path layout, cache namespace strings, or ambient
  `VX_RUNTIME_L2_DIR` lookup.
- The cache-dir APIs have been deleted: `eval_body_with_cache_dir`,
  `eval_body_with_exec_and_cache_dir`, internal `run_body_with_cache_dir`, and
  internal `run_body_with_exec_and_cache_dir` now appear only as guardrail
  needles. L2 behavior must cross the backend/service path.
- `vx-services::CasNode` now has a `CacheRecord` variant for portable
  cache-record bodies in `vx-store`, and `vx-services::CacheIndex` is the
  control-plane index surface mapping pre-run key digests to candidate cache
  record `NodeHash` values. `vx-daemon` hosts an in-process `CacheIndex` service
  whenever the orchestrator slot is configured.
- `vx-runtime::cache::RuntimeCache` no longer owns an
  `Option<LocalCacheIndex>` field and no longer imports `vx-cache-index`. It
  talks to a narrow persistent-layer adapter; caller-owned backends and the
  strand-backed service path live behind that slot.
- That persistent-layer adapter is now addressed by `(namespace,
  pre_run_input_key_digest)`. `RuntimeCache` owns the Facet JSON key digest and
  record encoding/decoding; adapters see only opaque record text.
- `vx-runtime::cache::RuntimeCacheBackend` is the public hook for a
  caller-owned persistent backend. `eval_body_with_cache_backend` and
  `eval_body_with_exec_and_cache_backend` install that backend without exposing
  `graph_exec` to daemon services.
- `vx-runtime::cache::RuntimeCacheService` is the runtime-owned async cache
  service abstraction. `vx-runtime::cache::StrandCacheBackend` implements the
  sync `RuntimeCacheBackend` by parking the worker strand on an `AwaitingCache`
  reason; the strand boss drives the service future and wakes the strand.
- `vx-daemon::CacheIndexRuntimeCacheService` is the first daemon-side
  `RuntimeCacheService` implementation. It stores opaque runtime record bodies
  as `vx-store` `CasNode::CacheRecord` nodes and indexes those records through
  `vx-services::CacheIndex` by `(namespace, pre_run_input_key_digest)`.
- `vx-runtime` must not import daemon cache service clients to make that happen:
  `CacheIndex`, `Store`, CAS cache-record nodes, and async RPC driving belong to
  the daemon/orchestrator or to a typed strand park path, not to `graph_exec`.
- `vx-runner` now implements the runtime `Observe` service and routes `"Observe"`
  through `RunnerAcceptor`; `vx-daemon` also advertises and routes `"Observe"` to
  its hosted `RunnerService` when a runner is configured. The implementation
  calls `run_exec`, converts the process result to `CompletedRun`, and invokes
  the shared observer-evaluation helper.
- Runner-backed top-level tree observe results cross the protocol as
  `Observed::Tree`; compound observer values containing output-tree leaves cross
  as `Observed::ValueWithTree`. Both install a CAS-backed `Producing` service
  from flushed `ExecResult.outputs`; both `RunnerAcceptor`
  and daemon-hosted runner sessions route `"Producing"`, and the service resolves
  demanded paths by walking `DirectoryNode` trees and fetching file chunks from
  `vx-store`. The old scratch-backed `exec_output_tree` runtime integration file
  has been deleted, and `exec_on_strands` no longer runs demanded execs through
  a runtime-local observe helper: its remaining runtime test is the cold
  non-demand proof that intentionally never reaches observe. The old
  `ScratchObserveService`, `scratch_dir`, `capture_outputs`, and
  `local_observer()` helper have also been deleted from `vx-runtime`. Direct
  observe-on-strands, output-tree, and exec cache coverage has moved to
  runner-owned tests, with live-VFS proofs ignored by default.
- Top-level producing trees returned by observe cross as remote handles attached
  to graph `OutputTree` nodes. Demanding `tree.file(path)` is a separate graph
  boundary that materializes only that path through strand remote-part demand.
  Public value reconstruction preserves those handles: a graph node with
  `NodeOutput::OutputTree` plus a registered producing handle becomes
  `Value::Tree(handle)`, not the executor-local `Value::OutputTree` marker. The
  executor-local marker is sealed behind a private-field type with a
  crate-private constructor.
- Grammar-package imports lower to graph lazy records whose fields are grammar
  coordinate graph values. Named projection follows graph projection resolution
  before choosing field slots, so chained import/tool projections stay typed by
  graph node identity rather than by source fallback.
- `where` capability constraints lower to graph boundary nodes with typed
  runtime metadata. Demand on that boundary selects an advertised provider,
  demands only provider attributes needed by the predicate, and memoizes the
  selected provider as graph output.
- Boundary payload demand is tokenized and input-authorized: the driver returns
  a non-clone `BoundaryDemand` when a boundary is reached, and runtime boundary
  materializers may borrow that token to create request-shaping payload demands
  only for the boundary node's declared graph inputs. Nested request-shaping
  field demands are authorized from `ResolvedBoundaryPayload`, produced by
  joining a direct `BoundaryPayloadDemand` with the matching resolved payload
  node, not from a bare `NodeId` or unrelated `ResolvedNode`. There is no public
  free `DemandRequest::root(node)` or
  `DemandRequest::boundary_payload(node)` constructor. Public value field demand
  is tokenized through resolved-node handles, and test-probe demand construction
  is cfg(test)-only.
- `KernelMode` no longer exposes `Off`, `Interp`, or `Jit` as old
  tree-walker-oracle labels. The public variants are graph-native policy labels:
  `GraphOnly`, `GraphKernelInterp`, and `GraphKernelJit`. The source-blind
  guardrail binary scans runtime, runner, and daemon Rust code to keep the old
  labels from returning.
- The old `vx-runtime::eval` source tree-walker and `dispatch.rs` scalar planner
  are deleted. The remaining `Thunk` type is an API-edge graph/native boundary
  wrapper with only `Done`, `Native`, and `Forcing` states; it cannot capture
  source expressions. Its native-callback constructor is crate-private, and the
  source-blind guardrail binary scans `eval.rs` to keep that constructor from
  becoming public again.

Latest verified red/green shape:

- `cargo check -p vx-exec-protocol -p vx-runtime -p vx-runner -p vx-daemon`:
  passed after making top-level producing-tree observe results explicit
  `Observed::Tree` protocol results.
- `cargo nextest run -p vx-runtime -E 'test(observe::tests::) | test(strand::tests::)' --no-fail-fast`:
  10/10 passed, including default `run.ok.out`, explicit `run.stream.out`, and
  strand handle-crossing coverage.
- `cargo nextest run -p vx-runner --no-fail-fast`: 6/6 passed, 11 skipped
  (the live-VFS proof targets).
- `cargo nextest run -p vx-exec-protocol -p vx-runtime -p vx-runner -p vx-daemon --no-fail-fast`:
  161/161 passed, 12 skipped, after making top-level producing-tree observe
  results explicit.
- `cargo check -p vx-exec-protocol -p vx-runtime -p vx-runner -p vx-daemon`:
  passed after wiring `Observed::ValueWithTree` nested output-tree crossing.
- `cargo nextest run -p vx-runtime -E 'binary(nested_tree_observe)' --no-fail-fast`:
  1/1 passed, proving a Vixen backtick observer result shaped as
  `Nested { out: run.stream.out }` can later demand `nested.out.file("out")`
  through the opened `Producing` handle.
- `cargo nextest run -p vx-runtime -E 'test(observe::tests::) | test(strand::tests::)' --no-fail-fast`:
  11/11 passed, including `Observed::ValueWithTree` helper coverage and strand
  handle-crossing coverage.
- `cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails)' --no-fail-fast`:
  10/10 passed.
- `cargo nextest run -p vx-exec-protocol -p vx-runtime -p vx-runner -p vx-daemon --no-fail-fast`:
  163/163 passed, 12 skipped, after nested output-tree crossing.
- `cargo nextest run -p vx-runner -E 'test(producing_vconn_serves_runner_output_tree_from_cas) | test(value_with_tree_observe_installs_runner_output_tree_from_cas)' --no-fail-fast`:
  2/2 passed, proving both top-level `Observed::Tree` and compound
  `Observed::ValueWithTree` runner outputs install the CAS-backed `Producing`
  service before a path is forced over the vconn.
- `cargo nextest run -p vx-runner --no-fail-fast`: 7/7 passed, 11 skipped, after
  adding runner-side `Observed::ValueWithTree` Producing coverage.
- `cargo nextest run -p vx-exec-protocol -p vx-runtime -p vx-runner -p vx-daemon --no-fail-fast`:
  164/164 passed, 12 skipped, after the runner-backed `ValueWithTree`
  Producing branch was wired.
- `cargo check -p vx-runtime -p vx-runner -p vx-daemon`: passed after making
  `Value::OutputTree` carry a sealed executor-local marker.
- `cargo check -p vx-runtime -p vx-runner -p vx-daemon`: passed after making
  graph public-value reconstruction preserve registered producing-tree handles.
- `cargo check -p vx-runtime -p vx-runner -p vx-daemon`: passed after making
  `Thunk::native` crate-private and moving native-boundary laziness coverage
  into `eval` unit tests.
- `cargo check -p vx-runtime -p vx-runner -p vx-daemon`: passed after renaming
  `KernelMode` variants to graph-native policy labels.
- `cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime`:
  passed after boundary payload demand was restricted to declared boundary
  inputs and `ResolvedBoundaryPayload` fields.
- `cargo check -p vx-runtime-driver -p vx-runtime`: passed after moving
  boundary payload demand to `BoundaryPayloadDemand` tokens minted only from
  declared boundary inputs or `ResolvedBoundaryPayload` fields, and after moving
  API-edge field demand to `PublicValueDemand` tokens minted from resolved-node
  handles.
- The forbidden-symbol scans from `01-guardrails.md` over
  `crates/vx-runtime-core`, `crates/vx-runtime-driver`, and
  `crates/vx-runtime/src` returned no matches after this checkpoint.
- `cargo nextest list -p vx-runtime -E 'binary(source_blind_guardrails)'`:
  selected ten source-blind guardrail tests, including
  `kernel_mode_oracle_labels_stay_deleted` and
  `native_thunk_constructor_stays_crate_private` and
  `executor_output_tree_marker_stays_sealed`.
- `cargo nextest run -p vx-runtime-driver --no-fail-fast`: 14/14 passed,
  including `resolved_public_value_authorizes_only_its_field_nodes`,
  `boundary_request_does_not_leave_node_in_progress`, and
  `boundary_payload_field_is_bound_to_resolved_payload_output`.
- `cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails)' --no-fail-fast`:
  10/10 passed, including `execution_crates_stay_source_blind` with guardrail
  needles for public free `root`, `boundary_payload`, `public_value_field`, and
  `test_probe` constructors, plus `kernel_mode_oracle_labels_stay_deleted` and
  `native_thunk_constructor_stays_crate_private`, plus
  `executor_output_tree_marker_stays_sealed`.
- `cargo nextest run -p vx-runtime --no-fail-fast`: 144/144 passed, including
  `lang_suite`.
- `cargo nextest run -p vx-runtime -E 'binary(lang_suite)' --no-fail-fast`:
  1/1 passed after the language suite switched to `GraphOnly`,
  `GraphKernelInterp`, and `GraphKernelJit`.
- `cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime --no-fail-fast`:
  163/163 passed, including `boundary_payload_field_is_bound_to_resolved_payload_output`,
  source-blind guardrails, and `lang_suite`.
- `cargo nextest run -p vx-runtime -p vx-runner -p vx-daemon --no-fail-fast`:
  159/159 passed, 12 skipped, after sealing the executor-local output tree
  marker.
- `cargo check -p vx-cache-index -p vx-exec-protocol -p vx-services -p vx-store -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner -p vx-daemon`
  passes with no old-evaluator dead-code warnings.
- `cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails) | binary(exec_on_strands)' --no-fail-fast`:
  8/8 passed, including `undemanded_exec_in_tuple_does_not_dispatch`,
  `runtime_cache_file_index_stays_out_of_graph_exec`, and
  `graph_exec_does_not_own_cache_index_details`, and
  `runtime_cache_uses_a_persistent_layer_adapter`, and
  `runtime_cache_does_not_import_service_clients`. The basic `run.ok`
  success/failure value-crossing tests, identical-demanded-backtick L1 reuse,
  writable-root disambiguation, observer-identity disambiguation,
  declared-tool-fingerprint disambiguation, executable-fingerprint
  disambiguation, and exec disk-L2 reuse moved to runner-owned live-VFS proof
  coverage.
- `cargo nextest run -p vx-runtime -E 'binary(host_probe)' --no-fail-fast`:
  6/6 passed, including the input-keyed `host.exec` L1 cache test, bare
  executable-name fingerprint resolution, and disk-L2 reuse across fresh graph
  evaluations.
- `cargo nextest run -p vx-runtime -E 'binary(bundle)' --no-fail-fast`:
  3/3 passed, including stable, content-sensitive observer bundle identity.
- `cargo nextest run -p vx-runtime -E 'binary(remote_tree_via_eval)' --no-fail-fast`:
  4/4 passed, including `graph_returned_remote_tree_stays_a_handle`, proving a
  graph-returned remote tree reconstructs as `Value::Tree(handle)`.
- `cargo nextest run -p vx-runtime -E 'binary(lang_suite)' --no-fail-fast`:
  1/1 passed; the `.vx` cases include `exec_boundaries.vx`, where an undemanded
  backtick exec in a lazy tuple slot stays cold with no observe client.
- `cargo nextest run -p vx-runtime -E 'binary(cc_probe)' --no-fail-fast`:
  1/1 passed.
- `cargo nextest run -p vx-runtime-driver --no-fail-fast`: 12/12 passed,
  including poison memoization, boundary in-progress cleanup, call-node
  non-strict argument edges, zero-arg direct call memo reuse, resolved-argument
  call memo reuse, resolved-argument disambiguation, closure capture memo reuse,
  function-entry disambiguation for shared code identities, and proof that
  unresolved arguments or captures are not demanded to build a call memo key.
- `cargo nextest run -p vx-runtime -E 'test(function_output_cache_backend) | test(function_output_l2_reuses_scalar_result)' --no-fail-fast`:
  2/2 passed, proving both the explicit local L2 and injected cache backend
  reuse scalar graph function outputs across fresh graph executions.
- `cargo nextest list -p vx-daemon -E 'test(cache_index_store_backend)'`:
  selected `runtime_cache::tests::cache_index_store_backend_reuses_function_output_through_strand_parking`.
- `cargo nextest run -p vx-daemon -E 'test(cache_index_store_backend)' --no-fail-fast`:
  1/1 passed, proving a strand-installed runtime cache backend can persist and
  reuse a graph function output through daemon `CacheIndex` plus `vx-store`
  `CacheRecord` bodies without importing service clients into `vx-runtime`.
- `cargo nextest run -p vx-runtime --no-fail-fast`: 145 run, 145 passed,
  0 failed, 0 skipped.
- `cargo nextest run -p vx-runner --no-fail-fast`: 6 run, 6 passed, 11 skipped
  (the ignored live-VFS proof targets).
- `cargo nextest list -p vx-runner --run-ignored only`: selected
  `tests::bare_invocation_failure_poisons_through_runner_backed_live_vfs`,
  `tests::declared_tool_fingerprint_disambiguates_runner_backed_l1_entry`,
  `tests::demanded_vixen_exec_runs_through_runner_backed_live_vfs_and_serves_output_tree`,
  `tests::executable_fingerprint_disambiguates_runner_backed_l1_entry`,
  `tests::explicit_stream_observer_returns_runner_backed_live_output_tree`,
  `tests::hello_world_uses_runner_backed_live_vfs_and_where_injection`,
  `tests::identical_backtick_exec_boundaries_share_runner_backed_l1_entry`,
  `tests::observer_identity_disambiguates_runner_backed_l1_entry`,
  `tests::run_ok_observer_crosses_runner_backed_live_vfs_for_success_and_failure`,
  `tests::strand_observe_uses_runner_backed_live_vfs_for_success_and_failure`,
  and `tests::writable_roots_disambiguate_runner_backed_l1_entry`.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::identical_backtick_exec_boundaries_share_runner_backed_l1_entry)' --no-fail-fast`:
  failed in the runner live-test helper before process execution because the
  helper passed `/tmp/vx/vfs` directly to `boot_with_store`; on macOS the daemon
  path must use `vx_vfsd::effective_mount_path` so the extension is reused at
  `/Volumes/Vixen`. This is a test/helper wiring gap, not permission to replace
  the path with scratch execution.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::writable_roots_disambiguate_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same stale runner-helper CAS-VFS boot boundary, before any
  scratch fallback could run.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::observer_identity_disambiguates_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same stale runner-helper CAS-VFS boot boundary, before any
  scratch fallback could run.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::declared_tool_fingerprint_disambiguates_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same stale runner-helper CAS-VFS boot boundary, before any
  scratch fallback could run.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::executable_fingerprint_disambiguates_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same stale runner-helper CAS-VFS boot boundary, before any
  scratch fallback could run.
- `cargo nextest list -p vx-daemon --run-ignored only -E 'test(=runtime_cache::tests::backtick_exec_cache_index_store_backend_survives_fresh_runner_backed_graph_eval)'`:
  selected the service-backed exec L2 proof.
- `cargo nextest run -p vx-daemon --run-ignored only -E 'test(=runtime_cache::tests::backtick_exec_cache_index_store_backend_survives_fresh_runner_backed_graph_eval)' --no-fail-fast`:
  failed at the same stale live-helper CAS-VFS boot boundary, before any scratch
  fallback or cache-dir fallback could run.
- `cargo nextest list -p vx -E 'binary(exec_e2e)' --run-ignored all`:
  selected `vx::exec_e2e exec_runs_against_the_vfs_and_flushes_the_write_set`.
- `cargo nextest run -p vx -E 'binary(exec_e2e)' --run-ignored ignored-only --no-fail-fast`:
  1/1 passed on macOS, proving the daemon-hosted path can reuse the live
  `/Volumes/Vixen` FSKit mount, run through runner exec, flush an output root,
  mount the flushed tree, and read it back. This proof still does not cover CAS
  input mounts; the test says input tree mounting is a follow-on.
- `cargo nextest list -p vx-daemon -p vx-store -E 'test(cache_index) | test(cache_record)'`
  selected the three cache seam proofs:
  `cache_index_rejects_empty_namespaces`,
  `cache_index_returns_records_for_the_pre_run_key`, and
  `cache_record_nodes_round_trip_through_store`.
- `cargo nextest run -p vx-daemon -p vx-store -E 'test(cache_index) | test(cache_record)' --no-fail-fast`:
  3/3 passed.
- `cargo nextest run -p vx-cache-index --no-fail-fast`: 2/2 passed, covering
  typed-key and digest-addressed local records.
- `cargo nextest run -p vx-cache-index -p vx-exec-protocol -p vx-services -p vx-store -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner -p vx-daemon --no-fail-fast`:
  184 run, 184 passed (1 leaky:
  `vx-runtime analyze::tests::a_demanded_bomb_is_reached`), 12 skipped.
- `cargo nextest run -p vx-runtime -p vx-daemon -p vx-runner --no-fail-fast`:
  155 run, 155 passed, 12 skipped after deleting all cache-dir runtime APIs
  and `vx-runtime`'s `vx-cache-index` dependency.
- `cargo nextest run -p vx-daemon --no-fail-fast`: 5/5 passed, 1 skipped.
- `cargo check -p vx-exec-protocol -p vx-services -p vx-runtime -p vx-runner -p vx-daemon -p vx`:
  passed after adding `create_dirs` to the observe/runner payload.
- `cargo nextest list -p vx-runtime -E 'binary(exec_create_dirs) | test(exec_invocation_metadata_carries_writable_roots_and_create_dirs)'`:
  selected `vx-runtime analyze::tests::exec_invocation_metadata_carries_writable_roots_and_create_dirs`
  and `vx-runtime::exec_create_dirs demanded_exec_forwards_create_dirs_to_observe_request`.
- `cargo nextest run -p vx-runtime -E 'binary(exec_create_dirs) | test(exec_invocation_metadata_carries_writable_roots_and_create_dirs)' --no-fail-fast`:
  2/2 passed, proving `.vx` exec metadata carries `create_dirs` and demanded
  graph exec forwards it to `ObserveRequest`.
- `cargo nextest run -p vx-exec-protocol -p vx-services -p vx-runtime -p vx-runner -p vx-daemon -p vx --no-fail-fast`:
  194 run, 194 passed, 16 skipped.
- `cargo check -p vx-exec-protocol -p vx-runtime -p vx-runner -p vx-daemon -p vx`:
  passed after adding string env entries to the observe payload.
- `cargo nextest list -p vx-runtime -E 'binary(exec_create_dirs) | test(exec_invocation_metadata_carries_env_entries) | test(exec_invocation_metadata_carries_writable_roots_and_create_dirs)'`:
  selected the two exec metadata unit tests plus the create-dirs/env
  `exec_create_dirs` integration tests.
- `cargo nextest run -p vx-runtime -E 'binary(exec_create_dirs) | test(exec_invocation_metadata_carries_env_entries) | test(exec_invocation_metadata_carries_writable_roots_and_create_dirs)' --no-fail-fast`:
  4/4 passed, proving `.vx` exec metadata carries env entries and demanded graph
  exec forwards dynamic env key/value pairs to `ObserveRequest`.
- `cargo nextest list -p vx-runtime -E 'binary(exec_create_dirs) | test(exec_invocation_metadata_carries_mounts) | test(exec_invocation_metadata_carries_env_entries) | test(exec_invocation_metadata_carries_writable_roots_and_create_dirs)'`:
  selected the three exec metadata unit tests plus the create-dirs/env/mounts
  `exec_create_dirs` integration tests.
- `cargo nextest run -p vx-runtime -E 'binary(exec_create_dirs) | test(exec_invocation_metadata_carries_mounts) | test(exec_invocation_metadata_carries_env_entries) | test(exec_invocation_metadata_carries_writable_roots_and_create_dirs)' --no-fail-fast`:
  6/6 passed, proving `.vx` exec metadata carries CAS Tree mounts and demanded
  graph exec forwards them to `ObserveRequest.mounts`.
- `cargo nextest run -p vx-exec-protocol -p vx-services -p vx-runtime -p vx-runner -p vx-daemon -p vx --no-fail-fast`:
  196 run, 196 passed, 16 skipped.
- The raw forbidden-symbol scans over `vx-runtime-core`, `vx-runtime-driver`,
  the deleted tree-walker affordance scan over `vx-runtime/src`, and the
  runtime-owned exec/observe protocol, cache file-index, and direct
  `graph_exec.rs` cache-index-detail scans returned no matches. The runtime
  cache service-client scan
  `rg -n 'vx_services|vx-services|StoreClient|CacheIndexClient|CacheLookupRequest|CachePutRequest|CacheRecordNode|CacheRecordBody|CacheInputKeyDigest|CasNode::CacheRecord|\.put_node\(|\.get_node\(' crates/vx-runtime/src crates/vx-runtime/Cargo.toml`
  also returned no matches.
- `vx-runner::tests::strand_observe_reaches_runner_backed_service` drives
  `vx-runtime` strands against a runner-backed `Observe` client and reaches the
  explicit no-CAS-VFS poison. This proves the strand/Observe seam can target the
  runner service without a runtime-to-runner dependency.
- `vx-runner::tests::strand_observe_uses_runner_backed_live_vfs_for_success_and_failure`
  is an ignored live-VFS proof target for direct Observe. It boots
  `vx_vfsd::boot_with_store`, runs `/bin/true` and `/bin/false` through
  `RunnerService::observe`, evaluates `(run) => run.ok`, and proves both the
  clean value and gate poison cross back through the strand boss. The old
  `crates/vx-runtime/tests/observe_on_strands.rs` scratch-backed integration
  file has been deleted.
- `vx-runner::output_tree::tests::reads_declared_output_root_from_cas` proves a
  flushed CAS output root can serve `out/y.txt` over the runner `Producing`
  service. `vx-runner::tests::producing_vconn_serves_runner_output_tree_from_cas`
  proves `RunnerAcceptor` routes the `"Producing"` virtual connection to that
  CAS-backed service.
- `vx-runner::tests::demanded_vixen_exec_reaches_runner_backed_observe` and
  `vx-runner::tests::undemanded_vixen_exec_with_runner_backed_client_stays_cold`
  compile real `.vx` backtick invocation shapes against a runner-backed observe
  client. The demanded case reaches the explicit no-CAS-VFS runner poison; the
  undemanded tuple-slot case stays cold. These do not yet prove live CAS-VFS
  execution from a demanded `.vx` backtick invocation.
- `vx-runner::tests::demanded_vixen_exec_runs_through_runner_backed_live_vfs_and_serves_output_tree`
  is an ignored live-VFS proof target. It compiles a real `.vx` backtick
  invocation, boots `vx_vfsd::boot_with_store`, registers a runner-backed
  `ObserveClient` from `RunnerService::with_vfsd`, runs `/bin/sh -c ...` with
  `writable: ["out"]`, and demands `out.file("out/y.txt")` through the
  runner-backed `Producing` virtual connection. It is skipped by default because
  it requires the host CAS-VFS mount.
- `vx-runner::tests::explicit_stream_observer_returns_runner_backed_live_output_tree`
  is the ignored live-VFS proof for the explicit `(run) => run.stream.out`
  observer shape moved out of `crates/vx-runtime/tests/exec_output_tree.rs`.
  It runs a real `.vx` backtick through `RunnerService::with_vfsd` and demands
  `out.file("out/y.txt")` through the returned runner-backed tree.
- `vx-runner::tests::bare_invocation_failure_poisons_through_runner_backed_live_vfs`
  is the ignored live-VFS proof for the default `run.ok.out` failure gate moved
  out of `crates/vx-runtime/tests/exec_output_tree.rs`; `/bin/sh -c "exit 7"`
  must poison through the runner-backed path.
- `vx-runner::tests::run_ok_observer_crosses_runner_backed_live_vfs_for_success_and_failure`
  is the ignored live-VFS proof moved out of `exec_on_strands` for the explicit
  `(run) => run.ok` observer value crossing. A real `.vx` backtick over
  `/bin/true` must return the crossed `Exit` struct, and `/bin/false` must
  return the gate poison, both through the runner-backed CAS-VFS path.
- `vx-runner::tests::identical_backtick_exec_boundaries_share_runner_backed_l1_entry`
  is the ignored live-VFS proof moved out of `exec_on_strands` for identical
  demanded backtick L1 reuse. It counts runner-backed `Observe` RPCs at the Vox
  service boundary while delegating to `RunnerService::with_vfsd`; two identical
  demanded invocations must dispatch observe once.
- `vx-daemon::runtime_cache::tests::backtick_exec_cache_index_store_backend_survives_fresh_runner_backed_graph_eval`
  is the ignored live-VFS proof for exec L2 reuse across fresh graph
  evaluations through the service-backed cache path. It counts runner-backed
  `Observe` RPCs while sharing a strand-installed `StrandCacheBackend` backed by
  `CacheIndexRuntimeCacheService`; the second fresh graph eval must reuse the
  persisted `vx-store` `CacheRecord` instead of dispatching observe again.
- `vx-runner::tests::writable_roots_disambiguate_runner_backed_l1_entry`
  is the ignored live-VFS proof moved out of `exec_on_strands` for writable-root
  L1 disambiguation. It uses identical command argv and different declared
  `writable` roots; the runner-backed `Observe` boundary must dispatch twice.
- `vx-runner::tests::observer_identity_disambiguates_runner_backed_l1_entry`
  is the ignored live-VFS proof moved out of `exec_on_strands` for observer
  bundle L1 disambiguation. It uses identical command argv and different
  observer bundles; the runner-backed `Observe` boundary must dispatch twice.
- `vx-runner::tests::declared_tool_fingerprint_disambiguates_runner_backed_l1_entry`
  is the ignored live-VFS proof moved out of `exec_on_strands` for declared
  tool-fingerprint L1 disambiguation. It uses identical command argv and
  different declared tool fingerprints; the runner-backed `Observe` boundary
  must dispatch twice.
- `vx-runner::tests::executable_fingerprint_disambiguates_runner_backed_l1_entry`
  is the ignored live-VFS proof moved out of `exec_on_strands` for executable
  fingerprint L1 disambiguation. It changes the host executable symlink between
  two demanded boundaries at the same path; the runner-backed `Observe` boundary
  must dispatch twice.
- `vx-runner::tests::hello_world_uses_runner_backed_live_vfs_and_where_injection`
  is the ignored live-VFS proof moved out of
  `crates/vx-runtime/tests/hello_world.rs`. It exercises `where` provider
  injection, a demanded backtick using the injected C compiler, declared
  `writable: ["out"]`, and `out.file("out/hello.o")` through the runner-backed
  CAS-VFS path. The old scratch-backed runtime integration file has been
  deleted.
- `vx-runtime::analyze::tests::exec_invocation_metadata_carries_writable_roots_and_create_dirs`
  proves `writable: ["out"]` and `create_dirs: ["out/obj"]` are recorded as exec
  metadata rather than being accepted only by the front-end.
- `vx-runtime::analyze::tests::exec_invocation_metadata_carries_env_entries`
  proves string env entries are recorded as exec metadata with graph node refs for
  key/value payload.
- `vx-runtime::analyze::tests::exec_invocation_metadata_carries_mounts` proves
  `mounts: %{ "src" => tree }` is recorded as exec metadata with graph node refs
  for the mount path and content-addressed Tree payload.
- `vx-runtime::exec_create_dirs::demanded_exec_forwards_create_dirs_to_observe_request`
  proves a demanded `.vx` backtick forwards `create_dirs` to `ObserveRequest`.
- `vx-runtime::exec_create_dirs::demanded_exec_forwards_env_to_observe_request`
  proves a demanded `.vx` backtick forwards dynamic env key/value payload to
  `ObserveRequest.env`.
- `vx-runtime::exec_create_dirs::demanded_exec_forwards_mounts_to_observe_request`
  proves a demanded `.vx` backtick forwards a resolved CAS Tree mount to
  `ObserveRequest.mounts`.
- The forbidden-symbol scan over `crates/vx-runtime-core` and
  `crates/vx-runtime-driver` returns no matches.
- The deleted tree-walker affordance scan over `crates/vx-runtime/src` returns
  no matches.

Verification commands:

```bash
cargo check -p vx-cache-index -p vx-exec-protocol -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner -p vx-daemon
cargo nextest run -p vx-cache-index -p vx-exec-protocol -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner
cargo nextest run -p vx-daemon
cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails)'
rg -n 'vx[_-]ir|ExprKind|StmtKind|PatternKind|VirProgram|BodyId|fn force|\.force\(|eval_inner|eval_expr' crates/vx-runtime-core crates/vx-runtime-driver
rg -n 'struct Evaluator|impl Evaluator|fn eval_inner|fn force_node|ThunkState::Unforced|ThunkState::Node|enum NodeMemo|struct NodeRef|Value::Closure|pub mod dispatch|crate::dispatch|DispatchPlan|struct Dispatcher|dispatch_scalar|force_frame_binding' crates/vx-runtime/src
```

The last two commands should return no matches. If either returns a match, fix
that before adding language behavior.

## Roadmap overview

1. **Demolition cut**: Public runtime entry points use the graph driver. The old
   expression walker is unreachable or deleted. Source entry points are cut over;
   body/observer entries remain. Expect honest red tests.
2. **Guardrails**: Expand crate-local tripwires into repo-level checks for
   forbidden dependencies and tree-walker affordances.
3. **Executable graph payloads**: Replace display labels and source ids with
   structured node payloads that are sufficient for execution.
4. **Value model cutover**: Orchestrator values are node references; resolved
   payloads live in node memo/output state.
5. **Pure graph execution**: Rebuild pure language semantics through graph nodes:
   constants, products, projections, params, kernels, conditionals, variants,
   match, calls, closures, strings, and failure propagation.
6. **Boundaries**: Rebuild imports, `where`, host operations, command literals,
   and exec as boundary nodes with legal boundary materialization origins.
7. **Cross-executor runtime**: Port observer shipping, wire crossing, remote tree
   handles, and strand parking onto graph driver semantics.
8. **Caching**: Add per-run memo, function memo, boundary L1/L2 input-keyed
   caching, and read-set provenance.
9. **Mode cleanup**: Remove tree-walker oracle language from runtime and tests.
10. **Final deletion**: Remove remaining expression-walker-only helpers and tests.

The detailed phase documents are:

- [01 - Guardrails](01-guardrails.md)
- [02 - Demolition Cut](02-demolition-cut.md)
- [03 - Executable Graph Payloads](03-executable-graph-payloads.md)
- [04 - Value Model And Pure Execution](04-value-model-and-pure-execution.md)
- [05 - Boundaries, Cross-Executor Runtime, And Cache](05-boundaries-cross-executor-cache.md)
- [06 - Verification And Resume Protocol](06-verification-and-resume.md)
- [07 - Exec/Observe Production Morph](07-exec-observe-morph.md)

## Resume protocol after compaction

When resuming this task after context compaction:

1. Read this file and the phase file that matches the current work.
2. Verify the checkpoint with `cargo check`, `cargo nextest`, and the forbidden
   symbol scan.
3. Inspect `vx-runtime::eval` before assuming the tree-walker is gone.
4. Continue the earliest incomplete phase. If the work is exec/observer related,
   read `07-exec-observe-morph.md` before touching code.
5. Commit each known state. A known state may be honestly red if the failure is
   explicit graph unsupported behavior and not a fallback.

## What not to do

- Do not add a `legacy_eval` fallback.
- Do not compare graph execution to the tree-walker as an oracle.
- Do not add `vx-ir` to `vx-runtime-core` or `vx-runtime-driver`.
- Do not add `ExprId` back into executable frames.
- Do not store `node_exprs` or `exprmap` in execution crates.
- Do not implement a general `force` API to make tests pass.
- Do not use labels for semantics once structured payloads exist.
