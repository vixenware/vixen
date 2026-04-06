# 007: Build demand-driven runtime on ExecutableProgram

A new runtime that consumes `ExecutableProgram` with demand-driven semantics.

## Design

### Slot model

ProgramEvaluator uses `BTreeMap<SlotId, EvalValue>` (eager).
The demand-driven runtime uses `BTreeMap<SlotId, SlotValue>` where:

```rust
enum SlotValue {
    Ready(EvalValue),
    Thunk(SharedThunk),  // deferred computation
}
```

### Thunk representation

Like vx-eval-tests `SharedThunk(Rc<RefCell<ThunkInner>>)`, but instead of
wrapping AST expressions, thunks wrap `(BodyId, Vec<EvalValue>)` — a thunk
body + captured values. Forcing a thunk executes its body.

### What to take from each evaluator

From **vx-eval-tests** (AST-walker):
- SharedThunk model (Rc<RefCell<ThunkInner>>, Ready/Suspended, memoized force)
- Lazy product fields (tuple elements are thunks, forced on access)
- Lazy pattern matching (bind fields as thunks, don't force until needed)

From **ProgramEvaluator** (vx-daemon):
- Slot-based KernelOp execution
- AwaitRuntime for exec/discover boundaries
- Memoization by (BodyId, Vec<EvalValue>)
- Async integration for process spawning

### Where does it live?

New crate. Start sync-only for pure computation. Add async boundary layer
when exec/discover support is needed (can wrap sync thunk layer).

The AST-walker in vx-eval-tests stays as a semantic oracle / regression suite.
ProgramEvaluator in vx-daemon stays as a demo shim until replaced.

### Async question

Thunk forcing should be sync (pure computation). Boundary crossing (exec/discover)
needs async. Strategy: sync thunk evaluation core, with async boundary handlers
that resolve to values before continuing.

## Depends on

006 (thunk KernelOps — ExecutableProgram must carry thunk bodies)

## Status

Not started.
