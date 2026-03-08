# Vixen runtime

This document covers how the Vixen runtime implements the language semantics
defined in `language.md`. The language design is about what things mean; this
document is about how they work.

## Value semantics implementation

The language promises value semantics: every value can be used as many times
as you want, passing a value to a function does not consume it, and there is
no way to observe mutation from another binding.

The naive implementation — deep-copy on every assignment and function call —
is obviously impractical. A `Tree` representing a large source directory
would be copied on every function call.

### Strategy: persistent data structures with reference counting

The runtime represents compound values (arrays, maps, structs, trees) as
persistent (structurally-shared) data structures, reference-counted at the
node level.

**What this means:**

- An `Array` is backed by a balanced tree (e.g., RRB tree or similar). Cloning
  an array is O(1) — it increments a reference count on the root. Appending or
  replacing an element is O(log n) — it creates new nodes along the path from
  root to the modified position, sharing the rest.

- A `Map` is backed by a hash-array mapped trie (HAMT) or similar persistent
  hash map. Same properties: O(1) clone, O(log n) insert/update.

- A `struct` is a fixed-size product type. Cloning is shallow — each field is
  a reference-counted handle. Updating one field creates a new struct node
  with the new field and shared references to the others.

- A `Tree` (store type) is already structurally shared by nature — it's a
  merkle tree in the content-addressable store. Cloning is O(1) (copy the
  root hash). Subtree operations are O(1) (slice the tree at a path).

**Implications for `!` rebinding:**

`items.append!("new")` is O(log n), not O(n). It creates a new path in
the persistent array, sharing the unchanged parts. The rebinding replaces
the root reference. This makes idiomatic Vixen code (chains of `!` calls)
efficient by default.

**Scalars** (U64, I64, F64, Bool) are unboxed values. They're copied
directly — no reference counting needed.

**Strings** are reference-counted byte buffers. Small strings may be
inlined (small-string optimization). String concatenation produces a new
buffer.

**Closures** are a function pointer plus a tuple of captured values. The
captured values are reference-counted handles, same as struct fields.
Cloning a closure is shallow.

### Cycle-free by design

Because recursive types are disallowed, the value graph is always a DAG.
Reference counting cannot leak — there are no cycles to break. No garbage
collector is needed.

## Dataflow evaluation

The language says: pending values look like normal values, and evaluation
only pauses where the actual resolved value is needed. There is no `await`.

The execution model is **dataflow**, not task-based. The runtime does not
run a single thread of execution that suspends when it hits a pending value.
Instead, it builds a dependency graph from the program and schedules nodes
whose inputs are ready.

### The dependency graph

The evaluator analyzes a function body and builds a graph:

- Each **binding** is a node.
- Each **use of a binding** is an edge from the defining node to the
  consuming node.
- A node is **ready** when all its inputs are resolved.

```vixen
fn build_both(src: Tree) -> Result[Tree, BuildError] {
  lib = compile_lib(src);
  tests = compile_tests(src);
  Tree.of({ "lib": lib, "tests": tests })
}
```

This produces three nodes:

```
src (input, already resolved)
  ├─→ lib = compile_lib(src)
  └─→ tests = compile_tests(src)
         ├─→ Tree.of({ "lib": lib, "tests": tests })
         │
  lib ───┘
```

`lib` and `tests` are both ready immediately (their only input, `src`, is
resolved). The runtime schedules both. `Tree.of` has two inputs that are
not yet resolved, so it waits. When both complete, `Tree.of` becomes ready
and runs.

There is no single thread of execution that "enters `build_both`, calls
`compile_lib`, suspends, calls `compile_tests`, suspends, then resumes."
The runtime sees three independent computations and schedules them by
their dependencies.

### Value states

Every value slot is in one of three states:

- **Resolved** — the value is available. Downstream nodes that depend on
  it can check this input off.
- **Pending** — the producing node hasn't completed yet. Downstream nodes
  that depend on it are not yet ready.
- **Poisoned** — the producing computation failed. Downstream nodes that
  depend on it will also become poisoned when they would otherwise become
  ready.

### How the scheduler works

The scheduler maintains a **ready queue** of nodes whose inputs are all
resolved. It pulls nodes from the queue and evaluates them, potentially
on different threads. When a node completes:

1. Its output slot transitions from pending to resolved.
2. All downstream nodes are checked — if all their inputs are now resolved,
   they move to the ready queue.

This is the same model as a spreadsheet recalculation engine, or an
incremental computation framework like picante. No coroutines, no
continuations, no green threads.

### Graph construction

The dependency graph is built by **static analysis of the function body**.
The evaluator doesn't need to look through function calls — it only needs
to see which bindings each statement references:

```vixen
a = f(x);      // depends on: x
b = g(y);      // depends on: y
c = h(a, b);   // depends on: a, b
```

`a` and `b` are independent (they share no inputs that are pending). `c`
depends on both. This is visible from the syntax alone — no interprocedural
analysis required.

For function calls, the runtime doesn't need to know what `f` does
internally. It treats `f(x)` as a node that takes `x` as input and
produces a result. If `f` itself contains parallelizable code, that's a
separate graph constructed when `f`'s body is evaluated.

### Loops

For loops are unrolled into the graph. Each iteration becomes a node (or
subgraph):

```vixen
for crate_spec in workspace.crates {
  compile_crate(src, crate_spec)
}
```

Each iteration depends on `src` and one element of `workspace.crates`.
No iteration depends on another, so they're all independent and can run
concurrently.

But if the loop body uses `!` rebinding:

```vixen
acc = 0;
for item in items {
  acc += process(item);
}
```

Each iteration depends on `acc` from the previous iteration. The graph
edges enforce sequential execution — no special-casing needed.

### No explicit concurrency primitives

There is no `spawn`, no `async`, no `await`, no channels. Parallelism
comes from the dependency graph. The user writes sequential-looking code
that describes what depends on what. The runtime recovers the parallelism.

See `concurrency.md` for more concrete examples.

## Exec internals

`exec` runs an external process on an executor with a virtual filesystem.

### Mount

A `Mount` describes how a store object appears in the executor's VFS:

```
Mount = struct {
  source: Blob | Tree,
  writable: Bool,
};
```

- **Read-only mount** (`writable: false`, the default) — the content is
  available in the VFS but the process cannot modify it. Most inputs
  (source files, dependencies, toolchains) are mounted read-only.

- **Writable mount** (`writable: true`) — the process can write to this
  location. Used for output directories. The observer can `commit` paths
  under writable mounts to capture outputs.

Mounts are specified as a map from `Path` (the VFS location) to `Mount`.
All paths are relative to the working directory — no absolute paths.

### Process

The `Process` handle available inside the observer:

```
Process = struct {
  stdout: Stream[Blob],
  stderr: Stream[Blob],
  exit_code: U64,       // pending until the process exits
};
```

- `stdout` and `stderr` are streams of chunks (blobs). The observer can
  read them incrementally as the process runs.
- `exit_code` is a pending value that resolves when the process exits.
  The observer can yield events before the process exits (streaming
  pattern) or wait for `exit_code` to resolve first (simple pattern).

The observer does not have access to stdin — processes are not interactive.
There is no signal API — if the observer needs to stop a process, the
runtime handles it (e.g., when the build is cancelled or a timeout fires).

### Arg and env types

Exec arguments and environment values must carry store references so the
runtime knows what to materialize in the VFS:

```
ExecArg = String | Path;
```

A plain `String` is a literal argument. A `Path` carries a store reference —
when it appears in an argument, the runtime ensures the referenced content
is available in the VFS at that path.

String interpolation with a `Path` produces an `ExecArg`, not a plain
`String`:

```vixen
arg = `--extern=dep=${rmeta}`;  // ExecArg, carries rmeta's store ref
plain = `--jobs=4`;              // String, no store refs
```

Environment values follow the same pattern:

```
env: Map[String, ExecArg]
```

This resolves the open question from `language.md` — the exact types for
args and env values are `Array[ExecArg]` and `Map[String, ExecArg]`.

### Commit and process lifecycle

`commit(path)` returns a lazy `Blob` handle. The observer can call `commit`
while the process is still running — but the path must exist and be stable
at the time the handle is demanded (i.e., when it crosses the yield
boundary back to the orchestrator).

In practice this means:

- **Streaming pattern**: the observer watches stdout for structured messages
  (e.g., rustc's `--json` output) that announce "file X is complete." The
  observer calls `commit` on the announced path, then yields an event
  carrying the handle. The yield forces materialization.

- **Simple pattern**: the observer waits for `exit_code`, then commits the
  output directory. Everything is stable because the process has exited.

There is no filesystem watching built into the runtime. The observer is
responsible for knowing when files are ready, typically by parsing process
output. This keeps the model simple — the observer is the interpreter
between the raw process and the structured build graph.

### Executor lifecycle

An executor stays alive as long as:

1. The process is still running, OR
2. There are unmaterialized `commit` handles from that executor

Once the process exits and all yielded handles are materialized, the
executor is released. This is why the yield boundary is the materialization
point — it ensures the executor doesn't need to hold file data indefinitely.
