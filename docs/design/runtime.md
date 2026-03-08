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

## Suspension and scheduling

The language says: pending values look like normal values, and evaluation
suspends when a pending value is actually needed. There is no `await`.

### What is a pending value?

At runtime, every value slot can be in one of three states:

- **Resolved** — the value is available. Reading it proceeds immediately.
- **Pending** — the value is not yet available. It is backed by a slot that
  will be filled when the producing computation completes.
- **Poisoned** — the producing computation failed. Touching the value
  propagates the error.

A pending value is a handle to a slot, same as a resolved value is a handle
to data. The language doesn't distinguish them — the runtime does.

### What happens when code touches a pending value?

The evaluator runs as a **task scheduler**. Each independent chain of
computation is a task. When a task touches a pending value:

1. The task **suspends**. Its current state (local bindings, position in the
   function body) is saved as a continuation.
2. The task is added to the pending value's **waiters list**.
3. The scheduler picks another runnable task.
4. When the pending value resolves, all waiters are made runnable again.

This is cooperative scheduling — tasks yield at pending-value boundaries, not
at arbitrary points. There is no preemption.

### Where can suspension happen?

Anywhere a value is inspected:

- Reading a field: `x.name` suspends if `x` is pending.
- Function call: `f(x)` suspends if `x` is pending (arguments must resolve
  before the call proceeds).
- Comparison: `x == y` suspends if either is pending.
- Pattern matching: `match x { ... }` suspends if `x` is pending.
- String interpolation: `` `${x}` `` suspends if `x` is pending.

Suspension can happen inside `for` loop bodies, inside closures passed to
`.map()`, anywhere. The task boundary is the unit of suspension, not the
function boundary.

### What is a task?

A task is a lightweight execution context — closer to a coroutine than a
thread. Tasks are multiplexed onto a thread pool. The runtime decides how
many tasks to run concurrently based on available parallelism and executor
capacity.

Tasks are created implicitly by the evaluator when independent work is
discovered. For example, if a function computes `a = f(x); b = g(y);`
where `f` and `g` have no data dependency, the runtime may evaluate them
as separate tasks. This is an optimization, not a language-level concept —
the program behaves the same whether `f` and `g` run concurrently or
sequentially.

### No explicit concurrency primitives

There is no `spawn`, no `async`, no `await`, no channels. Parallelism
comes from two sources:

1. **Data independence** — the scheduler runs independent computations in
   parallel when possible.
2. **`exec`** — each exec runs on an executor, potentially a separate
   machine. Multiple execs can run concurrently.

The user writes sequential-looking code. The runtime extracts parallelism
from the dependency structure.

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
