# Runtime graph execution - verification and resume protocol

This document is self-contained. It defines how to verify the graph-runtime
rebuild and how to resume the task after context compaction.

## Complete task context

The runtime must stop executing Vixen by walking source expressions. Analyzer and
lowerer may inspect VIR, but execution crates must be source-blind. The intended
shape is graph driver plus graph core: external demand enters through sealed
requests, driver schedules dependencies, core steps nodes and reports needs, and
node outputs are memoized by the driver. Unsupported graph shapes fail explicitly.

Current expected structural checkpoint: graph/core/driver crates exist, public
runtime entries go through graph execution, and the old `vx-runtime::eval`
source-expression tree-walker plus `dispatch.rs` scalar planner are deleted.

## After-compaction resume checklist

1. Read `docs/design/runtime-graph-execution/README.md`.
2. Read the phase file matching the active work.
3. Run:

   ```bash
   git status --short
   cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
   cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
   cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails)'
   ```

4. Run the forbidden-symbol scan:

   ```bash
   rg -n 'vx[_-]ir|ExprKind|ExprId|StmtKind|PatternKind|PatternId|VirProgram|BodyId|node_exprs|exprmap|eval_inner|eval_expr|walk_expr|legacy_eval|fn force|\.force\(' crates/vx-runtime-core crates/vx-runtime-driver
   rg -n 'struct Evaluator|impl Evaluator|fn eval_inner|fn force_node|ThunkState::Unforced|ThunkState::Node|enum NodeMemo|struct NodeRef|Value::Closure|pub mod dispatch|crate::dispatch|DispatchPlan|struct Dispatcher|dispatch_scalar|force_frame_binding' crates/vx-runtime/src
   ```

5. Inspect `crates/vx-runtime/src/eval.rs` before assuming no API-edge thunk
   cleanup remains.
6. Continue the earliest incomplete phase. If the work is exec/observer related,
   the active phase is `07-exec-observe-morph.md`: move scratch observe toward
   runner-backed production execution, not another runtime-local fake.
7. Commit the known state before switching tasks.

## Verification layers

### Compile verification

Minimum:

```bash
cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
```

When runtime API changes touch downstream crates, widen to workspace check.

### Test verification

Minimum:

```bash
cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
```

For a red demolition phase, record:

- failing test names
- whether each failure is `UnsupportedNode`, `Boundary`, `Poison`, or old-source
  fallback
- why the red state is honest

Do not mark a red state acceptable if any failure path uses `eval_inner`.
Do not rebuild a deleted eager evaluator, scratch process runner, source fallback,
or oracle path just to make the suite green. Some tests are scaffolding: if a test
keeps the wrong architecture alive, change or quarantine it in the same committed
state as the cut and record the reason.

### Forbidden-symbol verification

Core/driver scan must be clean:

```bash
rg -n 'vx[_-]ir|ExprKind|ExprId|StmtKind|PatternKind|PatternId|VirProgram|BodyId|node_exprs|exprmap|eval_inner|eval_expr|walk_expr|legacy_eval|fn force|\.force\(' crates/vx-runtime-core crates/vx-runtime-driver
```

The repo-level guardrail test is:

```bash
cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails)'
```

It scans `crates/vx-runtime-core/src`, `crates/vx-runtime-core/Cargo.toml`,
`crates/vx-runtime-driver/src`, and `crates/vx-runtime-driver/Cargo.toml` for
source-blindness. It also scans `crates/vx-runtime/src` for the deleted
tree-walker affordances and asserts `crates/vx-runtime/src/dispatch.rs` remains
absent. It also guards the current scratch observe implementation from being
presented as the real executor `ObserveService`. Add future execution crates to
the guarded list before relying on this check for them.

### Runtime-path verification

During demolition, inspect these paths:

```bash
rg -n 'run_in_mode|eval_body|eval_body_wire|eval_body_with_exec|eval_inner|Evaluator::eval|\.eval\(' crates/vx-runtime/src crates/vx-runtime/tests
```

Expected long-term:

- public runtime entry points call graph driver
- `eval_inner` is deleted
- tests do not instantiate an old evaluator as engine

Current partial state:

- `run`, `run_with_dispatches`, and `run_in_mode` call graph driver.
- `eval_body`, `eval_body_with_exec`, `eval_body_with_exec_and_providers`, and
  `eval_body_wire` call graph body execution. `eval_body_with_exec` passes its
  observe client to graph boundary materialization.
  `eval_body_with_exec_and_providers` passes its provider registry to graph
  capability boundary materialization.
- Direct and known closure calls leave source-blind `NodeKind::Call` wrappers
  around expanded body roots. Core demands only the body-root input; argument
  inputs are retained for graph identity and future call-memo keys without being
  strict call-boundary dependencies.
- Direct and closure call nodes carry a code identity and share a per-run driver
  call memo keyed by function entry, code identity, and already-resolved
  argument/capture outputs before the body runs. Unresolved arguments or
  captures are not demanded to build the key.
- Source-derived direct and closure identities are content-sensitive hashes of
  structured body material plus transitive identities for referenced function
  items (`blake3:<hash>` for functions or `closure:blake3:<hash>` for
  closures), not the earlier `fn:name` / source-span placeholders. Mutually
  recursive functions share one SCC fingerprint; the call node's function entry
  disambiguates memo keys.
- Self-contained function outputs (`U64`, `Bool`, `String`, `Unit`) persist to
  the opt-in local disk L2 when the call key is available without demanding
  unresolved arguments/captures. Graph-local product/variant/tree outputs remain
  per-run only.
- Observer execution calls graph body execution and graph/native wire crossing.
- Mirror, host, intrinsic, observer, observe-on-strands, backtick exec,
  exec-output-tree, remote-tree body, import/toolchain, and requested-toolchain
  capability tests exercise graph body paths.
- Boundary payload demand is authorized by a non-clone `BoundaryDemand` returned
  from the driver on `DriverError::Boundary`; materializers borrow it to create
  request-shaping payload demands. There is no public free
  `DemandRequest::boundary_payload(node)` constructor.
- The `.vx` harness uses `run_in_mode`, so source-language tests now exercise the
  graph driver and fail explicitly where graph payloads are unsupported.
- The old `Evaluator`, `eval_inner`, `force_node`, expression-capturing
  `ThunkState::Unforced`, `Value::Closure`, and `dispatch.rs` scalar planner are
  deleted from compiled runtime code.

Latest focused verification:

- `cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner`
  passes with no old-evaluator dead-code warnings.
- `cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails)' --no-fail-fast`:
  3/3 passed.
- `cargo nextest run -p vx-runtime -E 'binary(exec_on_strands)' --no-fail-fast`:
  8/8 passed, including cold exec non-demand, exact backtick exec L1 reuse,
  observer-identity non-aliasing, executable-fingerprint non-aliasing,
  declared-tool-fingerprint non-aliasing, and disk-L2 reuse across fresh graph
  evaluations.
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
- `cargo nextest run -p vx-runtime --no-fail-fast`: 154 run, 154 passed,
  0 failed, 0 skipped.
- `cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner --no-fail-fast`:
  174 run, 174 passed, 0 skipped.
- `vx-runner::tests::strand_observe_reaches_runner_backed_service` drives a
  `vx-runtime` strand against a runner-backed `Observe` service and reaches the
  explicit no-CAS-VFS poison without importing `vx-runner` into `vx-runtime`.
- `vx-runner` implements the runtime `Observe` service and routes it through
  `RunnerAcceptor`; `vx-daemon` advertises and routes `"Observe"` to its hosted
  `RunnerService` when a runner is configured. Runtime exec tests still use
  `local_observer()`. Treat those green exec tests as compatibility evidence, not
  as proof that runner-backed observe is the production runtime path.
- The forbidden-symbol scan over `vx-runtime-core` and `vx-runtime-driver`
  returns no matches.
- The deleted tree-walker affordance scan over `vx-runtime/src` returns no
  matches.

### Vixen-language verification

The `.vx` suite is important because it expresses language behavior:

- `demand_basis.vx`
- `which_field_which_graph.vx`
- `bindings.vx`
- `calls.vx`
- `closures.vx`
- `control_flow.vx`
- `exec_boundaries.vx`
- `match.vx`
- `poisons.vx`
- `variants.vx`
- `strings.vx`
- `failures.vx`
- `error_union.vx`

But the suite is only valid once the harness runs graph driver. A `.vx` pass via
tree-walker fallback is not a pass for this rebuild.

## Phase completion evidence

Each phase should end with a commit and a short note in the commit message or
follow-up docs containing:

- what changed structurally
- which runtime paths are graph-driven now
- which tests pass
- which tests are red and why
- forbidden-symbol scan result
- whether any old eval code remains reachable

## Definition of done

The runtime graph rebuild is done when:

- Public runtime entry points execute through graph driver.
- `.vx` language suite runs through graph driver.
- `vx-runtime-core` and `vx-runtime-driver` cannot depend on VIR.
- Graph execution cannot recover source expressions.
- There is no generic internal `force(node) -> value` API.
- Unsupported language features fail as graph `Unsupported*`, not fallback.
- Pure language tests pass through graph driver.
- Boundary/exec/remote tests pass through graph driver.
- Cache keys are input-keyed and computed before running.
- `KernelMode` no longer names a tree-walker oracle.
- Old expression-walker-only helpers are deleted.

Final scan targets:

```bash
cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
rg -n 'eval_inner|legacy_eval|eval_expr|walk_expr|node_exprs|exprmap' crates/vx-runtime crates/vx-runtime-core crates/vx-runtime-driver
rg -n 'vx[_-]ir|ExprKind|ExprId|StmtKind|PatternKind|PatternId|VirProgram|BodyId|fn force|\.force\(' crates/vx-runtime-core crates/vx-runtime-driver
```

The first grep may still find analyzer/lowerer terms in `vx-runtime` until those
modules are split, but it must not find execution-path source fallback.
