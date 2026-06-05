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
- `KernelMode::Off` must not mean "tree-walker oracle." It is currently a
  transitional graph-entry mode and should eventually disappear or become an
  explicit graph-kernel policy.
- The bomb detonates only when the graph driver demands its node.
- Cache keys are input-keyed and computed before running; read-sets are
  post-run provenance, never key material.

## Current checkpoint to verify

Do not trust this paragraph as status; verify it in code before continuing.

Expected latest structural checkpoint:

- Commit `4c55d433` exists: `Fence graph execution away from VIR`.
- Commit `bfcc1493` exists: `Document graph runtime demolition roadmap`.
- `Cargo.toml` includes `crates/vx-runtime-graph`,
  `crates/vx-runtime-core`, and `crates/vx-runtime-driver`.
- `crates/vx-runtime-core/Cargo.toml` has no `vx-ir` dependency.
- `crates/vx-runtime-driver/Cargo.toml` has no `vx-ir` dependency.
- `crates/vx-runtime/src/graph.rs` only re-exports `vx_runtime_graph`.
- `crates/vx-runtime/tests/source_blind_guardrails.rs` scans
  `vx-runtime-core` and `vx-runtime-driver` source/manifests for forbidden
  source-evaluator affordances, scans `vx-runtime/src` for the deleted runtime
  tree-walker affordances, and guards the temporary scratch observe executor from
  being renamed back into a production-looking `ObserveService`.
- `docs/design/runtime-graph-execution/07-exec-observe-morph.md` records the
  active plan for turning the quarantined scratch observe path into the
  runner-backed production exec path instead of preserving it as a green-test
  fallback.
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
- The runtime L2 can persist self-contained function call outputs (`U64`, `Bool`,
  `String`, `Unit`) under that same call key when an explicit cache directory or
  `VX_RUNTIME_L2_DIR` is configured. Outputs containing graph-local node refs
  remain per-run only.
- `vx-runtime-driver` reports graph kernel executions for the transitional
  public dispatch counter.
- `graph_exec` can expose graph-root lazy arrays/tuples/variants and caught
  failures as old public `Value` thunks whose fields demand graph nodes through
  `PublicValueField`.
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
- Invocation `writable: [...]` roots lower into exec boundary metadata as graph
  node refs, are demanded only during boundary materialization, cross in
  `ObserveRequest.outputs`, are included in the exec cache key, and are consumed
  by `vx-runner` as `ExecRequest.outputs`.
- Observer execution uses graph body execution and graph/native wire crossing.
- `vx-runner` now implements the runtime `Observe` service and routes `"Observe"`
  through `RunnerAcceptor`; `vx-daemon` also advertises and routes `"Observe"` to
  its hosted `RunnerService` when a runner is configured. The implementation
  calls `run_exec`, converts the process result to `CompletedRun`, and invokes
  the shared observer-evaluation helper.
- Runner-backed `WireValue::Tree` observe results now install a CAS-backed
  `Producing` service from flushed `ExecResult.outputs`; both `RunnerAcceptor`
  and daemon-hosted runner sessions route `"Producing"`, and the service resolves
  demanded paths by walking `DirectoryNode` trees and fetching file chunks from
  `vx-store`. Default runtime exec/output tests still use `local_observer()` and
  therefore do not prove the full runner-backed `.vx` exec path; direct
  observe-on-strands coverage has moved to runner-owned tests, and live-VFS
  proofs currently exist as ignored runner-owned tests.
- Top-level producing trees returned by observe cross as remote handles attached
  to graph `OutputTree` nodes. Demanding `tree.file(path)` is a separate graph
  boundary that materializes only that path through strand remote-part demand.
- Grammar-package imports lower to graph lazy records whose fields are grammar
  coordinate graph values. Named projection follows graph projection resolution
  before choosing field slots, so chained import/tool projections stay typed by
  graph node identity rather than by source fallback.
- `where` capability constraints lower to graph boundary nodes with typed
  runtime metadata. Demand on that boundary selects an advertised provider,
  demands only provider attributes needed by the predicate, and memoizes the
  selected provider as graph output.
- Boundary payload demand is tokenized: the driver returns a non-clone
  `BoundaryDemand` when a boundary is reached, and runtime boundary
  materializers must borrow that token to create request-shaping payload demands.
  There is no public free `DemandRequest::boundary_payload(node)` constructor.
- The old `vx-runtime::eval` source tree-walker and `dispatch.rs` scalar planner
  are deleted. The remaining `Thunk` type is an API-edge graph/native boundary
  wrapper with only `Done`, `Native`, and `Forcing` states; it cannot capture
  source expressions.

Latest verified red/green shape:

- `cargo check -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner -p vx-daemon`
  passes with no old-evaluator dead-code warnings.
- `cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails)' --no-fail-fast`:
  3/3 passed.
- `cargo nextest run -p vx-runtime -E 'binary(exec_on_strands)' --no-fail-fast`:
  8/8 passed, including `undemanded_exec_in_tuple_does_not_dispatch`,
  exact-L1 reuse for identical demanded backtick invocations, observer identity
  non-aliasing, executable-fingerprint non-aliasing when a stable binary path
  changes content, declared-tool-fingerprint non-aliasing, and disk-L2 reuse
  across fresh graph evaluations.
- `cargo nextest run -p vx-runtime -E 'binary(exec_output_tree)' --no-fail-fast`:
  3/3 passed.
- `cargo nextest run -p vx-runtime -E 'binary(host_probe)' --no-fail-fast`:
  6/6 passed, including the input-keyed `host.exec` L1 cache test, bare
  executable-name fingerprint resolution, and disk-L2 reuse across fresh graph
  evaluations.
- `cargo nextest run -p vx-runtime -E 'binary(bundle)' --no-fail-fast`:
  3/3 passed, including stable, content-sensitive observer bundle identity.
- `cargo nextest run -p vx-runtime -E 'binary(remote_tree_via_eval)' --no-fail-fast`:
  3/3 passed.
- `cargo nextest run -p vx-runtime -E 'binary(lang_suite)' --no-fail-fast`:
  1/1 passed; the `.vx` cases include `exec_boundaries.vx`, where an undemanded
  backtick exec in a lazy tuple slot stays cold with no observe client.
- `cargo nextest run -p vx-runtime -E 'binary(cc_probe) | binary(hello_world)' --no-fail-fast`:
  2/2 passed.
- `cargo nextest run -p vx-runtime-driver --no-fail-fast`: 12/12 passed,
  including poison memoization, boundary in-progress cleanup, call-node
  non-strict argument edges, zero-arg direct call memo reuse, resolved-argument
  call memo reuse, resolved-argument disambiguation, closure capture memo reuse,
  function-entry disambiguation for shared code identities, and proof that
  unresolved arguments or captures are not demanded to build a call memo key.
- `cargo nextest run -p vx-runtime --no-fail-fast`: 155 run, 155 passed,
  0 failed, 0 skipped.
- `cargo nextest run -p vx-runner --no-fail-fast`: 6 run, 6 passed, 2 skipped
  (the ignored live-VFS proof targets).
- `cargo nextest list -p vx-runner --run-ignored only`: selected
  `tests::demanded_vixen_exec_runs_through_runner_backed_live_vfs_and_serves_output_tree`
  and
  `tests::strand_observe_uses_runner_backed_live_vfs_for_success_and_failure`.
- `cargo nextest run -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner --no-fail-fast`:
  187 run, 187 passed, 2 skipped.
- `cargo nextest run -p vx-daemon --no-fail-fast`: 2/2 passed.
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
- `vx-runtime::analyze::tests::exec_invocation_metadata_carries_writable_roots`
  proves `writable: ["out"]` is recorded as exec metadata rather than being
  accepted only by the front-end.
- `vx-runtime::exec_on_strands::writable_roots_are_part_of_the_backtick_exec_l1_key`
  proves two otherwise-identical backtick execs with different writable roots do
  not alias in the in-process exec L1.
- The forbidden-symbol scan over `crates/vx-runtime-core` and
  `crates/vx-runtime-driver` returns no matches.
- The deleted tree-walker affordance scan over `crates/vx-runtime/src` returns
  no matches.

Verification commands:

```bash
cargo check -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner -p vx-daemon
cargo nextest run -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner
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
