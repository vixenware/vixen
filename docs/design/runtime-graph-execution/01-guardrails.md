# Runtime graph execution - guardrails

This document is self-contained. It describes the structural constraints that
prevent the Vixen runtime from regrowing an eager source tree-walker.

## Complete task context

Vixen must execute a demand graph. A value in the orchestrator is a reference to a
graph node; a node memo records whether that node has resolved. Demand starts
from the external root result or from an environment boundary. Inside the graph,
node execution may request dependencies, but it must not synchronously force
arbitrary nodes. Lazy data is graph structure; eager pure compute is a kernel
island; effects are boundary nodes.

The failure mode is source-shaped execution: code imports VIR, matches
`ExprKind`, calls `eval_inner`, and pulls on subexpressions directly. That shape
was already reintroduced once by wrapping the tree-walker in graph frames. This
document records the walls that must make that impossible, or at least
mechanically visible.

Current expected checkpoint: `vx-runtime-graph`, `vx-runtime-core`, and
`vx-runtime-driver` exist. Core and driver have no VIR dependencies and have
source/manifests tripwires. The old `vx-runtime::eval` still exists and must be
cut off from public runtime paths in the demolition phase.

## Crate boundary

The split is intentional:

- `vx-runtime-graph` owns graph data types. It has no VIR dependency.
- `vx-runtime-core` steps one graph node given already-ready inputs. It has no
  VIR dependency, no queue, no memo table, and no external demand constructors.
- `vx-runtime-driver` owns legal demand origins, memo state, and scheduling. It
  can satisfy `Need` results from core. It has no VIR dependency.
- `vx-runtime` may still contain analyzer/front-end/lowering code that depends on
  VIR. Runtime execution should migrate out of it or call into driver.

This boundary must be defended in manifests, source, and tests.

## Forbidden in execution crates

Forbidden in `vx-runtime-core` and `vx-runtime-driver`:

- `vx_ir` or `vx-ir`
- `ExprKind`, `ExprId`
- `StmtKind`, `StmtId`
- `PatternKind`, `PatternId`
- `VirProgram`
- `BodyId`
- `ItemKind`
- `node_exprs`
- `exprmap`
- `eval_inner`
- `eval_expr`
- `walk_expr`
- `legacy_eval`
- generic `fn force`
- `.force(`

Some of those names may be valid in analyzer or lowerer modules. They are not
valid in source-blind execution crates.

## Allowed in execution crates

Allowed:

- `DemandGraph`, `NodeId`, `NodeKind`, structured graph payloads.
- `Step::Need`, `Step::Ready`, `Step::Boundary`, `Step::Poison`,
  `Step::Unsupported`.
- Driver-owned memo table.
- Driver-owned legal demand origins: root result, boundary materialization, test
  probe.
- Kernel IR and kernel interpreter once kernel nodes carry structured kernel
  references or payloads.
- Boundary request data once boundary nodes carry structured boundary payloads.

The key shape is:

```rust
core::step(node, ready_inputs) -> Step
driver::drive(DemandRequest) -> Result<NodeOutput, DriverError>
```

The forbidden shape is:

```rust
eval(expr, env) -> Value
force(node) -> Value
node_to_expr(node) -> ExprId
```

## Tripwire requirements

The crate-local tripwires are useful but not enough. The next guardrail work
should add a repo-level test or xtask check that scans:

- `crates/vx-runtime-core/src`
- `crates/vx-runtime-core/Cargo.toml`
- `crates/vx-runtime-driver/src`
- `crates/vx-runtime-driver/Cargo.toml`
- any future execution crate or module

It should fail on the forbidden symbol list above.

The check should be mechanically boring and hard to miss. A simple Rust test or
xtask command is acceptable. It is not a substitute for type boundaries, but it
catches the predictable "temporary fallback" edits.

## Dependency rules

`vx-runtime-core` dependency direction:

```text
vx-runtime-core -> vx-runtime-graph
```

`vx-runtime-driver` dependency direction:

```text
vx-runtime-driver -> vx-runtime-core
vx-runtime-driver -> vx-runtime-graph
```

Forbidden dependency direction:

```text
vx-runtime-core -> vx-ir
vx-runtime-driver -> vx-ir
vx-runtime-core -> vx-runtime
vx-runtime-driver -> vx-runtime
```

`vx-runtime` may depend on the graph/core/driver crates while migration is in
progress. The long-term goal is for source-facing work to feed executable graphs
into driver, not for driver to depend back on source-facing runtime code.

## Demand request sealing

`DemandRequest` constructors live in the driver. Core must not be able to create
external demand requests. Core can only report `Need(nodes)`; driver decides
whether satisfying that need is legal in the context of the original external
demand.

Do not make `DemandRequest` fields public. Do not add blanket conversion from
`NodeId` to `DemandRequest`. Do not expose `resolve_node` publicly as a generic
force operation.

## What counts as failure

Acceptable failure:

- A public runtime entry returns `UnsupportedNode` because a graph payload is not
  executable yet.
- A Vixen language test turns red after tree-walker demolition because calls,
  match, variants, exec, or closures are not yet represented in executable graph
  form.
- A boundary node returns `BoundaryRequest` because boundary execution is not
  wired yet.

Unacceptable failure:

- A test stays green because code silently fell back to `eval_inner`.
- A core/driver crate imports VIR.
- A graph node stores an `ExprId` so execution can look back into source.
- A helper named `force` appears in the graph execution path.

## Verification commands

Run before and after every phase touching runtime execution:

```bash
cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
rg -n 'vx[_-]ir|ExprKind|ExprId|StmtKind|PatternKind|PatternId|VirProgram|BodyId|node_exprs|exprmap|eval_inner|eval_expr|walk_expr|legacy_eval|fn force|\.force\(' crates/vx-runtime-core crates/vx-runtime-driver
```

