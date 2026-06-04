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

The old `vx-runtime::eval` tree-walker still exists. The next implementation work
is to remove it from public runtime paths and rebuild behavior through the graph
driver. Many tests may go red during that cut. Red `UnsupportedNode` failures are
acceptable. Green tests produced by expression interpretation are not.

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
- `KernelMode::Off` must not mean "tree-walker oracle." It should eventually mean
  graph interpreter with JIT disabled, or disappear.
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
  source-evaluator affordances.
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
- `vx-runtime-driver` reports graph kernel executions for the transitional
  public dispatch counter.
- `graph_exec` can expose graph-root lazy arrays/tuples as old public `Value`
  thunks whose fields demand graph nodes through `PublicValueField`.
- `vx-runtime::eval` still contains the legacy tree-walker for `eval_body*`,
  observer execution, mirror tests, and other not-yet-cut public body paths.

Verification commands:

```bash
cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
cargo nextest run -p vx-runtime execution_crates_stay_source_blind
rg -n 'vx[_-]ir|ExprKind|StmtKind|PatternKind|VirProgram|BodyId|fn force|\.force\(|eval_inner|eval_expr' crates/vx-runtime-core crates/vx-runtime-driver
```

The last command should return no matches. If it returns a match in core/driver,
fix that before adding language behavior.

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

## Resume protocol after compaction

When resuming this task after context compaction:

1. Read this file and the phase file that matches the current work.
2. Verify the checkpoint with `cargo check`, `cargo nextest`, and the forbidden
   symbol scan.
3. Inspect `vx-runtime::eval` before assuming the tree-walker is gone.
4. Continue the earliest incomplete phase.
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
