# Runtime graph execution - demolition cut

This document is self-contained. It describes the cut that removes the expression
tree-walker from public runtime execution paths.

## Complete task context

Vixen's runtime must be graph-driven. The analyzer can traverse VIR to construct
graph nodes. The executor/driver/core must not traverse VIR or recover source
expressions. The current repository has a structural start: graph/core/driver
crates exist and are source-blind. The old runtime path in `vx-runtime::eval`
still contains `eval_inner`, expression-capturing thunks, and source-shaped
helper methods. That code must stop being the engine.

The goal of this phase is not to keep the suite green. The goal is to make every
public execution path go through graph driver, with unsupported behavior reported
as graph unsupported. An honest red state is acceptable and useful if it proves
there is no fallback.

## Public entry points to cut over

Find and update every public path that currently invokes expression evaluation:

- `run`
- `run_with_dispatches`
- `run_in_mode`
- `eval_body`
- `eval_body_wire`
- `eval_body_with_exec`
- strand eval entry points
- exec observer evaluation
- mirror evaluation
- tests that instantiate `Evaluator` and call `eval`
- `.vx` language test harness through `run_in_mode`

Each path should compile a/an graph from the VIR front-end, then call a graph
driver entry.

The exact graph driver API may evolve, but the shape must remain:

```text
external request -> DemandRequest -> Driver::drive -> Step loop -> NodeOutput
```

There must not be a hidden fallback:

```text
UnsupportedNode -> eval_inner
```

## Expected immediate result

Many tests may fail, especially tests for:

- function calls
- closures
- pattern matching
- variants
- strings and methods
- imports
- `where`
- command literals
- exec observers
- remote tree methods
- failure context details

That is acceptable if failures are explicit graph failures. The failure reason
should name unsupported graph node/payload/boundary, not "unsupported expression"
from an expression interpreter.

## Demolition checklist

1. Add a graph-engine public entry in `vx-runtime` that:
   - runs the existing front-end,
   - builds a graph,
   - constructs `vx_runtime_driver::Driver`,
   - drives `DemandRequest::root(graph.root)`,
   - converts `NodeOutput` to the public `Outcome` shape temporarily.

2. Point `run_in_mode` at the graph-engine public entry.

3. Point `run` and `run_with_dispatches` at `run_in_mode` or the graph-engine
   entry.

4. Point `eval_body` and `eval_body_wire` at graph driver equivalents. If
   argument binding is not represented yet, fail explicitly as unsupported graph
   function parameters rather than invoking source eval.

5. Point exec observer evaluation at graph driver. If observer bundles are not
   executable graph payloads yet, return unsupported observer graph shape.

6. Remove direct calls to `Evaluator::eval` from tests. Tests may be rewritten to
   call graph driver helpers, or may fail honestly until graph payloads exist.

7. Make `eval_inner` private and unreachable, then delete it when all public
   paths are off it. If deletion is too large for the first demolition commit,
   add a tripwire that fails if any public path calls it.

8. Remove `KernelMode::Off` as "tree-walker disabled dispatch." Temporary modes
   can be:
   - graph interpreter
   - graph plus kernel interpreter
   - graph plus JIT

9. Delete comments that call tree-walk a ground-truth oracle.

10. Commit the known state. If red, document exact failing tests and confirm they
    fail with graph unsupported errors.

## What to preserve during demolition

Keep these semantics as targets even if not all are implemented immediately:

- Bomb poisons only when its node is demanded.
- Lazy data construction does not demand fields.
- Projection demands only selected slot.
- Kernel `if` demands condition before selected branch.
- Boundary payload materialization is a legal demand origin.
- Driver memoizes node outputs per run.

Keep these support pieces if useful, but do not let them pull source expressions:

- `Poison` and `FailureContext` data model.
- Wire value conversion types.
- Remote handle plumbing.
- Kernel interpreter/JIT code.
- Analyzer and lowerer code that traverses VIR.

## What to remove or quarantine

Remove from runtime execution:

- `ThunkState::Unforced { expr, env, body }`
- `node_exprs`
- expression-to-node reverse maps in execution frames
- `force_frame_binding`
- `eval_inner`
- `dispatch_scalar(e: ExprId, ...)`
- source-pattern matching as runtime matching
- function invocation that binds `ExprId` argument thunks

It is acceptable for analyzer/lowerer to retain expression maps while building
graphs. It is not acceptable for graph execution to retain them.

## Acceptance criteria

The demolition phase is complete when:

- `run_in_mode` cannot reach `eval_inner`.
- `.vx` harness cannot reach source expression evaluation.
- Core/driver forbidden-symbol scan is clean.
- Any remaining red tests fail through graph-driver unsupported/boundary/poison
  errors.
- There is no source-expression fallback path in public runtime execution.

## Verification commands

Use these after demolition:

```bash
cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime
rg -n 'run_in_mode|eval_body|eval_body_wire|eval_body_with_exec|eval_inner|Evaluator::eval|\.eval\(' crates/vx-runtime/src crates/vx-runtime/tests
rg -n 'vx[_-]ir|ExprKind|ExprId|StmtKind|PatternKind|PatternId|VirProgram|BodyId|node_exprs|exprmap|eval_inner|eval_expr|walk_expr|legacy_eval|fn force|\.force\(' crates/vx-runtime-core crates/vx-runtime-driver
```

