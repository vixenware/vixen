# Scheduler for vx-eval: demand-driven dataflow resolution

## Context

The vx-eval engine conflates graph building and graph resolution. The spec says
the orchestrator never forces pending values — operations on pending values
produce pending results, and forcing only happens at boundaries (exec
serialization, external caller). The current implementation violates this:
`execute_node`, builtins (`exec_builtin`, `array_merge`, etc.), and even
`eval_block` (non-last expressions) call `force_value` synchronously, blocking
the calling thread. Concurrency only happens through ad-hoc `thread::scope` in
`force_many`. The result: independent exec calls (like 20 `cc -c` invocations)
run serially instead of concurrently.

The fix is a proper scheduler that separates graph building from resolution,
discovers the dynamically expanding dependency graph, and runs independent
ready nodes concurrently.

## Step 1: Dependency extraction

Add methods to discover what `Pending(NodeId)` values a node depends on.

**Files:** `node.rs`, `value.rs`

- `Value::collect_pending(&self, deps: &mut Vec<NodeId>)` — walks a value
  recursively, collecting all `Pending` node IDs. Handles compound types
  (Array, Tuple, Map, Tree, Struct, Discovered).
- `NodeJob::pending_deps(&self) -> Vec<NodeId>` — extracts pending deps from
  the job's Value fields. For `If`, only the condition. For `Call`, the
  function + args + kwargs + kwargs_expr. For `Binary`, left + right. Etc.

**Tests:** Create nodes with various mixes of resolved/pending values, verify
deps are correctly extracted. Test compound values containing nested pending.

## Step 2: Scheduler struct and single-threaded resolution loop

New file `scheduler.rs`. The scheduler replaces `force_value` at the boundary.

```rust
struct Scheduler<'e> {
    engine: &'e Engine,
    // forward deps: node -> nodes it needs
    deps: HashMap<NodeId, Vec<NodeId>>,
    // reverse deps: node -> nodes waiting on it
    rdeps: HashMap<NodeId, Vec<NodeId>>,
    // nodes ready to execute (all deps resolved)
    ready: VecDeque<NodeId>,
    // for cycle detection
    in_demand: HashSet<NodeId>,
}
```

Core method: `resolve_value(value: Value) -> Result<Value, PoisonError>`

Loop:
1. Walk demanded value, collect all Pending node IDs into the demand set
2. For each pending node, compute `pending_deps()`, build dep/rdep edges
3. Recursively process newly discovered deps
4. Nodes with no pending deps → ready queue
5. Pop a ready node, execute it via `engine.execute_node_scheduled(node)`
6. Resolve/poison the node, notify waiters
7. Check rdeps: any node whose deps are now all resolved → ready queue
8. If execution produced a value with new Pending nodes (dynamic expansion),
   add them to the demand set and repeat from step 2
9. Loop until the demanded value is fully resolved

**Single-threaded first.** No thread pool yet — just the correct resolution
order. This alone fixes nothing for concurrency but establishes the correct
scheduling model.

**Tests:** Diamond deps (A→B,C→D), linear chains, dynamic expansion (Call that
creates new nodes), cycle detection, poison propagation.

## Step 3: `execute_node_scheduled` — non-forcing node execution

New method on Engine that executes a node whose immediate Value deps are
guaranteed resolved. It does NOT call `force_value`/`force_many`. For:

- **Binary/Unary**: inputs resolved, compute directly.
- **Template**: parts resolved, interpolate.
- **Field**: receiver resolved, select field.
- **If**: condition resolved, enter chosen branch via `eval_block`. Result may
  contain new Pending values — returned as-is for the scheduler.
- **Call/Invoke**: function resolved, enter body via `eval_block`/`invoke_function`.
  Result may contain new Pending values.
- **Fail**: produce poison.

The key difference from current `execute_node`: no `force_value`, no
`force_many`, no `force_template_parts`. Values are already resolved by the
scheduler's dependency tracking.

**Important subtlety**: Call nodes first need the function value resolved to
determine if it's a user function, partial, or builtin. The scheduler
guarantees the function Value is resolved. But entering the function body
(`invoke_function` → `eval_block`) creates new nodes and may return pending
values. That's fine — the scheduler handles them.

**Tests:** Execute each node variant with pre-resolved inputs, verify results.

## Step 4: Wire the scheduler into Engine::call

Replace `force_value(value)` in `Engine::call()` and `Engine::call_entry()`
with `Scheduler::new(self).resolve_value(value)`.

Keep the old `force_value` path alive temporarily (behind a flag or as a
fallback) so we can run existing tests against both paths.

All existing tests must pass. The scheduler produces identical results, just
via a different resolution strategy.

**Tests:** Run the full existing test suite against the scheduler path.

## Step 5: Make builtins scheduler-compatible

Builtins are the hardest part. Each builtin that calls `force_value` internally
needs to work under the assumption that its inputs are already resolved (because
the scheduler pre-resolved them).

Key builtins to fix:
- **`exec_builtin`**: `parse_exec_request` calls `force_value_shallow`,
  `force_value` on observer, executor, etc. Under the scheduler, all these are
  already resolved by the time exec runs. The force calls become no-ops (values
  pass through unchanged). Can be verified with assertions.
- **`array_map`**: currently forces receiver shallowly, then calls
  `execute_call` per item. Under the scheduler, it should return an Array of
  Pending call nodes. The scheduler discovers and resolves them.
- **`array_merge`**: currently forces each item sequentially. Under the
  scheduler, all items are resolved before merge runs (they're dependencies).
  The sequential force loop becomes a simple iteration over resolved values.
- **`array_flatten`**, **`map_map_values`**: same pattern.

**Strategy**: For each builtin, the force calls become identity operations on
already-resolved values. Gradually replace `force_value(x)` with `assert
resolved + use x directly`.

## Step 6: Handle eval_block non-last expression forcing

Line 1024: `eval_block` forces non-last `Stmt::Expr` values. This is for
side-effect expressions (e.g., a bare `exec(...)` call whose result is
discarded). Under the scheduler model, these values still need to be demanded
but not forced inline.

Options:
- Track "must-demand" values in the block's result and let the scheduler
  resolve them alongside the block's return value
- Add a `Value::Demanded` wrapper that the scheduler knows to resolve

This needs careful thought — a bare expression statement that's pending means
its side effects haven't happened. The scheduler must ensure it resolves before
the block's final value is used (since later statements may depend on those
side effects through the environment, not through value dependencies).

## Step 7: Handle spread forcing

Lines 1293, 1327: array/record spread calls `force_value_shallow` during
`eval_expr` because you need to know the value's type to spread it. This is
structural — you can't build the graph without knowing whether it's an array.

Options:
- Accept shallow forcing during graph building (it only resolves one level)
- Add Spread node types that the scheduler handles

Shallow forcing during graph building is probably acceptable — it resolves the
container but not its elements. The elements remain pending.

## Step 8: Add concurrency to the scheduler

Once the single-threaded scheduler works and all tests pass, add a fixed-size
worker thread pool. The scheduler's ready queue dispatches to workers via a
channel. Results come back via a result channel. The scheduler loop becomes:

1. Dispatch all ready nodes to worker threads
2. Wait on result channel for any completion
3. Process the result (resolve/poison, update deps, discover new ready nodes)
4. Repeat

```rust
// Worker threads
fn worker(engine: &Engine, rx: Receiver<Arc<Node>>, tx: Sender<(NodeId, Result<Value, PoisonError>)>) {
    while let Ok(node) = rx.recv() {
        let result = engine.execute_node_scheduled(&node);
        tx.send((node.id, result)).ok();
    }
}
```

Concurrency control: limit the number of in-flight exec nodes (configurable),
since each exec may spawn a process.

**Tests:** Timing-based test with Sleep nodes: N independent sleep(200ms) nodes
should complete in ~200ms, not N*200ms.

## Step 9: Replace cycle detection

Remove the thread-local `DEMAND_STACK`. The scheduler tracks cycles
structurally via its dependency graph. Before adding a dep edge A→B, check if
B transitively depends on A. If so, poison with circular dependency.

## Step 10: Remove old forcing code

Once all tests pass under the scheduler, remove:
- `force_value`, `force_value_shallow`, `force_many`, `force_node`
- `force_pending_shallow`, `force_selector_receiver`
- `force_template_parts`
- Thread-local `DEMAND_STACK`
- `thread::scope` usage in `force_many`

## Critical files

- `crates/vx-eval/src/worker.rs` — Engine, execute_node, eval_expr, builtins
- `crates/vx-eval/src/node.rs` — Node, NodeJob, NodeState
- `crates/vx-eval/src/value.rs` — Value enum
- `crates/vx-eval/src/scheduler.rs` — new file
- `crates/vx-eval/src/tests.rs` — existing tests + new scheduler tests
- `crates/vx-eval/src/lib.rs` — module declaration

## Verification

- All existing tests in `crates/vx-eval/src/tests.rs` pass
- `cargo nextest run -p vx-eval`
- New scheduler-specific tests pass (diamond deps, cycles, dynamic expansion,
  concurrent exec, poison propagation)
- Integration: `vx build` on a real project produces the same output
