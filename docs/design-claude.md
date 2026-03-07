# Vixen: Design Specification

A typed, demand-driven build language with a distributed memoizing runtime.

---

## Glossary

| Term | Definition |
|------|-----------|
| **Value** | A typed node in the computation graph. Source-level, it is an ordinary typed expression. Operationally, it may be unrealized until demanded. |
| **Binding** | A named reference to a value. Bindings name computation graph nodes, not eagerly realized products. |
| **Demand** | An external request to realize a value. All demand originates outside the language (CLI, API). The language has no force/await/realize construct. |
| **Demand root** | The top-level value the user asks for. Everything else is realized only as a transitive dependency. |
| **Tracked computation** | A computation whose inputs and outputs are recorded by the runtime for memoization and invalidation. |
| **Host builtin** | A primitive operation that crosses the pure/effect boundary: `exec`, `acquire`, `download`, `read`, `glob`, `write_file`, format parsing, hashing. |
| **Blob** | Immutable, pathless, content-addressed bytes in the store. Identity is BLAKE3 hash. |
| **Tree** | A first-class structural value: a content-addressed manifest of named entries (blobs and subtrees). Not `Map<Path, Blob>` — it carries membership, subtree identity, and merge semantics. |
| **Stream\<T\>** | An unordered, reusable, progressively-produced collection of typed elements. Elements arrive as the producer yields them. Multiple consumers observe the same cached elements. |
| **List\<T\>** | A concrete, ordered, finite collection. Spine is known immediately. Elements may be unrealized computation nodes. |
| **TreeEntry** | A typed stream element from tree-producing operations. Carries `path`, `content`, and `kind` (file or directory). |
| **Store** | Content-addressed storage (BLAKE3, CDC chunking, Merkle manifests). Currently local, designed for remote. The single source of truth for all artifacts. |
| **Orchestrator** | The process that evaluates Vixen, discovers tracked computations, and dispatches work. Lives close to the source tree. Currently `vxd`. |
| **Executor** | A process that runs sandboxed `exec` commands. Mounts inputs from the store via VFS. May be local or remote. Currently `vx-runner`. |
| **VFS** | Virtual filesystem that projects store content into executor sandboxes. FSKit on macOS, FUSE on Linux, ProjFS on Windows. Currently `vx-vfsd`. |
| **Lease** | A temporary hold on in-flight CAS objects during execution. Prevents GC from reclaiming blobs that have been uploaded but not yet published. Expires on execution failure or abandonment. |
| **Root** | A durable reference from a memo table entry or demand result to published CAS objects. Roots are what keep committed values alive across GC. |
| **Early cutoff** | If a computation reruns and produces a value identical to its previous result, downstream dependents are not invalidated. |
| **Monotonic producer** | A producer whose yielded elements are individually final — later elements cannot invalidate earlier ones. Archive extraction is monotonic; generic process execution is not. |

---

## 0. Non-goals

These are explicitly out of scope. They are not deferred features; they are things this system does not attempt.

- **General-purpose programming language.** Vixen is a build language. It has no mutation, no general IO, no networking (other than `download`/`acquire`), no threads, no FFI beyond `exec`.
- **Backward compatibility with any existing build system's rule format.** No Bazel WORKSPACE, no Makefile, no CMakeLists.txt compatibility layer.
- **Build-system-as-a-service API.** The system is a tool, not a platform. Remote execution is for distributing work, not for offering builds to third parties.
- **Transparent distributed filesystem.** The VFS exists to enforce sandboxing, not to provide a general-purpose network filesystem.
- **Perfect static termination guarantees.** `exec` can loop forever. The language allows unrestricted recursion. Cycle detection is runtime, not compile-time.
- **IDE-first language experience in v0.** The language needs a lossless CST and LSP eventually, but the runtime semantics come first.
- **Make it easy to write non-deterministic builds.** Every design choice biases toward determinism. Non-determinism is a bug, not a feature.

---

## 1. Motivation and framing

### 1.1 The problem

Build systems serve a deceptively simple purpose: given source files and tools, produce correct output artifacts, and do so incrementally — rebuilding only what changed.

The difficulty is that "what changed" and "correct" are entangled with dependency discovery, caching identity, environment control, and scheduling. Most build systems make fixed choices along these axes and then spend years working around the consequences.

Vixen starts from a different premise: **the build description is a program in a pure functional language, and the build system is a memoizing, demand-driven evaluator for that language.** Dependencies are discovered during evaluation. Scheduling falls out of data independence. Caching is value-based. The graph is a runtime trace, not a source-level authoring artifact.

### 1.2 Prior art, without caricature

**Make** is the origin point. It introduced file-based timestamps, static dependency declarations, and shell-based actions. Its weaknesses are well-known: no dynamic dependencies, no content-based invalidation, no sandboxing, implicit dependence on global system state. But its simplicity — a DAG of file-producing commands — remains the mental model most build tools descend from.

**Bazel** (and Buck2) bring content-addressed caching, sandboxed execution, remote execution, and a rule language that separates loading from analysis from execution. The architecture is powerful but introduces a rigid phase model: configuration → loading → analysis → execution. Dynamic dependencies require explicit expansion points (aspects, transitions, exec groups). The rule language is intentionally restricted to prevent users from breaking the phase model. The result is a system that is correct and fast for the shapes it anticipates, but fighting the framework for shapes it doesn't.

**Shake** is the build system that takes dynamic dependencies most seriously. It models builds as monadic computations where dependencies are discovered during execution via `need`. It supports early cutoff, tracks file content rather than just timestamps, and handles dynamic dependencies naturally. But Shake is a Haskell library, not a standalone system. It does not provide sandboxing, remote execution, or a content-addressed store. It is a scheduling strategy, not a complete build tool.

**Nix** treats the build as functional evaluation over derivations. Builds are content-addressed, hermetic, and reproducible. But Nix's evaluation and build execution are strictly separated: the Nix language evaluates to derivations (static descriptions), and then a separate builder executes them. Dynamic dependencies during a single build are awkward. The store is powerful but local-first, and distributed Nix builds are bolted on rather than native.

### 1.3 Where this sits in Build Systems à la Carte

Mokhov, Mitchell, and Peyton Jones (2018) classify build systems along two axes:

1. **Scheduler type**: topological (static), restarting (restart on new dependency discovery), or suspending (suspend when a dependency is needed, resume when it's ready).
2. **Rebuilder type**: dirty-bit, verifying traces (check recorded dependencies), or constructive traces (store enough to reconstruct without re-executing).

In their taxonomy:

| System | Scheduler | Rebuilder |
|--------|-----------|-----------|
| Make | Topological | Dirty bit (mtime) |
| Shake | Suspending | Verifying traces |
| Bazel | Restarting | Constructive traces |
| Nix | Topological | Constructive traces |

**Vixen is a suspending scheduler with constructive traces, extended in several directions the paper does not cover.**

Specifically:

- **Suspending scheduler.** Evaluation is demand-driven. When a computation needs a value that is not yet realized, evaluation suspends on that dependency and resumes when the value becomes available. Dependencies are discovered dynamically during evaluation, not declared ahead of time. This is closest to Shake's model.

- **Constructive traces.** Results are stored in a content-addressed store (BLAKE3, CDC chunking, Merkle manifests). A computation's result can be reconstructed from the store without re-executing if the cache key matches. This is closest to Nix/Bazel's approach.

#### Where the paper's taxonomy becomes insufficient

The paper models build systems as `Task c k v` — a function from a key `k` to a value `v`, parameterized by a constraint `c` on what the task can observe. This model assumes:

1. **Tasks are identified by keys.** In Vixen, "tasks" are not a first-class concept. There are computations — function applications over typed values. A computation's identity is its function identity plus its argument values, not a separate key namespace.

2. **Values are opaque.** The paper's `v` is an unstructured type. Vixen values are richly typed: `RustLibrary`, `Executable`, `Tree`, `Stream<TreeEntry>`. The type system participates in caching (early cutoff compares typed values, not just hashes), discoverability (demandable products are identified by type), and composition (typed outputs of one computation become typed inputs of the next).

3. **The store is a `k -> v` map.** Vixen's store is a content-addressed DAG of blobs and Merkle tree manifests. A "value" may be a structured tree whose identity is its Merkle root, not a single flat entry. Store identity and computation identity are related but not isomorphic.

4. **Execution is implicit.** The paper abstracts over how values are produced. Vixen distinguishes between pure computation (referentially transparent, can run anywhere) and host computation (`exec`, `acquire`) that requires orchestration — sandboxing, VFS projection, remote dispatch, lease management, output capture. The paper's model has no place for this distinction.

5. **Streaming does not exist.** The paper models each task as producing a single value. Vixen's `acquire` produces a `Stream<TreeEntry>` — a progressively-yielded collection whose elements can drive downstream computations before the producer is finished. This is outside the paper's `Task c k v` model entirely.

6. **There is no store lifecycle.** The paper does not model garbage collection, leases, or the distinction between in-flight and committed values. Vixen requires all three.

The paper remains the best available framework for classifying the core scheduling and rebuilding strategy. But Vixen's design extends into territory the paper intentionally leaves abstract: typed values, structured storage, sandboxed execution, streaming, and store lifecycle.

---

## 2. Core semantic model

### 2.1 What a program is

A Vixen program is a collection of modules. Each module exports types, values, and functions. There are no top-level side effects, no `main`, no `root` declarations. A module is a namespace of typed definitions.

```
// A module defines types, values, and functions.
// Nothing here triggers evaluation.

fn build_lib(pkg: RustPackage, tc: RustToolchain) -> RustLibrary {
  deps = pkg.deps.map(|dep| build_lib(dep, tc))
  exec(
    argv: [tc.rustc, "--edition", pkg.edition, "--crate-type", "lib", ...],
    inputs: { pkg.srcs, deps.map(|d| d.rlib) },
    outputs: { rlib: file("lib{pkg.crate_name}.rlib") },
  ).rlib
}

my_lib = build_lib(my_package, my_toolchain)
```

`my_lib` is a binding. It names a computation node. Nothing happens until something outside the language demands it.

### 2.2 Values

A value in Vixen is a typed node in the computation graph:

- **At the source level**, it is an ordinary typed expression: `RustLibrary`, `String`, `List<Object>`, `Stream<TreeEntry>`, `Tree`.
- **Operationally**, it is a node in a memoized dependency graph that may or may not be realized.
- **There is no type-level distinction between "realized" and "unrealized."** A `RustLibrary` is a `RustLibrary` whether it came from cache, was just compiled, or hasn't been realized yet.

This is the fundamental semantic commitment: **the language does not expose evaluation state.** There is no `Future<T>`, no `Tracked<T>`, no `Lazy<T>`. The runtime manages realization. The author thinks in values.

### 2.3 Pure computation and host computation

The language has two kinds of computation, but the distinction is invisible in the type system:

- **Pure computation**: function application, pattern matching, collection transforms, arithmetic. Referentially transparent. Can be evaluated anywhere, freely reordered, trivially memoized.
- **Host computation**: `exec`, `acquire`, `download`, `read`, `glob`, `write_file`, format parsing, hashing. These cross the effect boundary — they touch the store, the network, or a sandboxed subprocess. They are still semantically pure (same inputs → same outputs, modulo transient failures), but operationally they require orchestration.

Any function may call host builtins. There is no effect marker, no coloring. The runtime discovers which computations involve host effects through evaluation, not through declarations.

### 2.4 Dynamic dependencies and demand-driven evaluation

Dependencies are discovered during evaluation. There is no separate "declare all dependencies" phase.

```
fn build_crate(pkg: RustPackage, tc: RustToolchain) -> RustLibrary {
  // deps is computed from pkg — discovered at evaluation time
  deps = pkg.deps.map(|dep| build_crate(dep, tc))
  ...
}
```

Evaluation is demand-driven: the runtime evaluates only what is needed to produce the demanded value. If `build_crate` is never demanded, its body is never evaluated. If only some deps are needed (because of early cutoff or conditional logic), only those are realized.

This is the suspending scheduler from Build Systems à la Carte: when evaluation needs a value, it suspends and resumes when the value is available. The runtime is free to evaluate independent subcomputations concurrently.

### 2.5 Determinism

Vixen programs must be deterministic with respect to their inputs. Specifically:

- Pure computation is deterministic by construction.
- Host computation (`exec`) is deterministic by sandboxing: the executor can only observe declared inputs, a controlled environment, and a hermetic toolchain. If the sandbox is correct, the same inputs produce the same outputs.
- Streams are unordered — there is no completion-order nondeterminism in the language semantics. Collection points impose deterministic ordering.
- Failure tie-breaking is deterministic among already-observed failures.

The invariant is: **given the same source files, toolchains, and runtime semantics, the system produces the same outputs.** File watching, parallelism, and scheduling may vary, but the semantic result must not.

### 2.6 The semantic status of `exec`

`exec` is the hardest primitive to classify because it is simultaneously a pure function (semantically) and a scheduled effect (operationally).

**Semantically**: `exec(argv, env, inputs, outputs) -> TypedOutputRecord` is a function. Given the same argv, environment, input trees/files, and output declarations, it produces the same typed output record. Its result is stored in the content-addressed store and can be reused across machines. It participates in early cutoff: if the outputs are identical to a previous run, downstream dependents are not invalidated.

**Operationally**: `exec` requires the runtime to set up a sandbox, project inputs from the store via VFS, spawn a process, capture declared outputs back to the store, and manage leases during execution. This is scheduled work, not simple function application.

The language resolves this by treating `exec` as a function at every level the author can observe, and handling the operational complexity in the runtime. The author writes `exec(...)` and gets a typed result. The runtime schedules, sandboxes, caches, and distributes.

**What `exec` does not expose:**

- Provisional filesystem state during execution (temp files, renames, partial writes)
- stderr (runtime diagnostics, not a return value)
- Timings (runtime telemetry)
- Exit status (nonzero is a failure, not a result)

**What `exec` may expose if declared:**

- stdout as `Bytes` (only when `capture_stdout: true`; text decoding is a separate step)

### 2.7 Semantic values vs. runtime telemetry

The language draws a hard line between semantic values (things computations produce and consume) and runtime telemetry (things the runtime observes for operational purposes).

| Semantic value | Runtime telemetry |
|----------------|-------------------|
| `exec` declared output files/trees | stderr output |
| `acquire` stream elements | Download progress |
| Captured stdout (when declared) | Execution timings |
| Parsed TOML/JSON values | CAS upload events |
| Typed computation results | Lease acquisition/release |
| Error diagnostics | Scheduler decisions |

Telemetry may be surfaced in debugging tools, the graph inspector, or logging. It is never part of a value's identity, never participates in caching, and never flows into downstream computations.

---

## 3. Operational model

### 3.1 Architecture

Vixen's runtime is a multi-process system:

```
vx (CLI)
  │
  │ RPC (roam)
  ▼
vxd (orchestrator)
  │
  ├── RPC ──► vx-store (CAS)
  │
  ├── RPC ──► vx-runner (executor)
  │               │
  │               └── RPC ──► vx-vfsd (VFS daemon)
  │                               │
  │                               └── RPC ──► vx-store
```

**vxd (orchestrator)**: Evaluates Vixen modules, constructs the computation graph, dispatches `exec` work to runners, manages the memo table. Lives close to the source tree. One instance per workspace; parallel tests share it via lock file.

**vx-store**: Content-addressed blob store. BLAKE3 hashing, content-defined chunking for large blobs, Merkle tree manifests for directories. Deduplication is automatic. Currently local; designed for remote deployment.

**vx-runner (executor)**: Receives `exec` requests from `vxd`. Sets up sandboxes. Mounts inputs via VFS. Spawns processes. Captures outputs back to the store. May be local or remote.

**vx-vfsd (VFS daemon)**: Runs filesystem operations in a separate process from the runner. Reads content from `vx-store` on demand. Platforms: FSKit (macOS), FUSE (Linux), ProjFS (Windows).

### 3.2 Model A semantically, Model B operationally

The system maintains two faces:

**To the language**: everything is evaluation. `exec` is a function call. The result appears as a typed value. There is no "scheduler" or "executor" in the language's vocabulary.

**To the runtime**: `exec` is orchestrated work. The orchestrator maintains a work queue. It dispatches to executors. It manages leases. It handles executor failures. It throttles concurrency. It deduplicates in-flight requests for the same computation.

This split is deliberate. The language stays simple and pure. The operational complexity is contained in the runtime, invisible to authors.

The moment these two faces leak into each other — if the language needs to know about scheduling, or if the runtime needs to understand language constructs beyond the primitive boundary — the design has failed.

### 3.3 Sandboxing and VFS projection

When the orchestrator dispatches an `exec`:

1. The executor receives the computation specification: argv, environment, input mapping (trees and placed files), output declarations.
2. The VFS daemon mounts the declared inputs from the store. The process sees a filesystem containing exactly those files at the declared paths, nothing else.
3. The process runs. It can read its declared inputs. It can write to its declared output paths. It cannot access the network, the host filesystem, or other undeclared state.
4. On successful exit (code 0), the executor captures declared output files and trees from the sandbox back into the store. Each output is content-addressed.
5. On failure (nonzero exit), the executor reports the failure. Leased blobs may be reclaimed.

Sandboxing is enforced by the VFS (the process literally cannot see undeclared files) and by platform-specific mechanisms (Landlock on Linux, sandbox profiles on macOS).

### 3.4 Remote execution

The architecture is designed so that vx-runner can be remote:

- The orchestrator sends a computation specification over RPC.
- The remote runner accesses the store (which may also be remote, or replicated).
- Inputs are mounted from the store — no file shipping from orchestrator to executor.
- Outputs go back to the store — no file shipping from executor to orchestrator.
- The orchestrator only needs the output Merkle roots / blob hashes to proceed.

The store is the rendezvous point. Orchestrator and executor never exchange file contents directly. This is why everything is content-addressed and why the VFS reads from the store, not from the orchestrator.

### 3.5 Backpressure and scheduling

The orchestrator is implicitly a scheduler. Evaluation may discover thousands of independent `exec` nodes. The runtime must throttle:

- **Local concurrency limit**: max parallel exec processes per executor.
- **Remote concurrency limit**: max in-flight requests per remote runner pool.
- **Store bandwidth**: max concurrent uploads/downloads to CAS.

The language does not see this. From the language's perspective, independent computations may run concurrently. The runtime decides how much concurrency is actually realized.

This is where "Model A semantically, Model B operationally" is load-bearing. The scheduler exists; it just doesn't leak into the language.

---

## 4. Cache and identity model

### 4.1 What goes into a cache key

A computation's cache key must capture everything that can affect its output. For Vixen, this means:

| Component | What it captures |
|-----------|-----------------|
| **Function identity** | Which function is being called. In practice: a hash of the function's definition in the Vixen source. |
| **Argument values** | The content-addressed identity of every argument. For blobs: BLAKE3 hash. For trees: Merkle root. For typed values: structural hash of all fields. |
| **Host builtin identity** | For `exec`: the argv, environment, output declarations. For `acquire`: URL, expected hash. |
| **Runtime semantics version** | A version stamp for the runtime itself. If the semantics of `exec` sandboxing change, all cached `exec` results must be invalidated. |

What does NOT go into the cache key:

- Scheduler decisions (which executor, what order)
- Wall-clock time
- Hostname, PID, or other environmental noise
- Store location (local vs. remote)

### 4.2 Function identity

This is the hardest part. When a Vixen function's body changes, its cache key must change. But "the function changed" is not simple to define in a language with closures, higher-order functions, and recursion.

**Proposed approach**: hash the function's normalized AST (after parsing, before evaluation). Two functions with identical normalized ASTs have identical identity. A change to any expression in the function body changes the hash.

**Captured variables**: if a function closes over a value, that value's identity is part of the function's identity. This is necessary for correctness: `make_compiler(tc)` with different toolchains must produce different cache keys.

**Transitive closure**: if function `f` calls function `g`, and `g` changes, then `f`'s results may be invalid. This is handled by the dependency graph: `f`'s cache entry depends on `g`'s results. If `g` produces different results (detected by value comparison), `f` is re-evaluated. If `g` produces the same results (early cutoff), `f`'s cached result remains valid.

This means function identity does not need to transitively include callee identities. It only needs its own AST hash plus its argument values. Early cutoff handles the transitive case.

### 4.3 Value-based caching, concretely

"Value-based caching" means the cache key includes the identity of values, not the identity of how those values were produced.

Concrete example: **a Cargo.toml whose formatting changes but whose parsed content does not.**

```
// Step 1: Read the manifest file
raw_bytes = read_bytes(workspace.entry("Cargo.toml"))
// cache key: BLAKE3(Cargo.toml bytes)
// Cargo.toml was reformatted, so raw_bytes changes → step 1 reruns

// Step 2: Parse TOML
manifest = parse_toml(raw_bytes)
// cache key: function_id(parse_toml) + BLAKE3(raw_bytes)
// raw_bytes changed → step 2 reruns
// BUT: the parsed value is structurally identical to the previous run

// Step 3: Use the manifest to determine dependencies
deps = manifest.dependencies
// manifest is unchanged (early cutoff) → step 3 does NOT rerun

// Step 4: Compile
lib = compile(deps, ...)
// deps is unchanged → step 4 does NOT rerun
```

Early cutoff at step 2 prevents the formatting change from propagating. This is only possible because parsing and compilation are separate tracked computation boundaries with separate cache entries.

### 4.4 Invariants for sound memoization

1. **Determinism**: given the same cache key, a computation must produce the same value. Sandboxing enforces this for `exec`.
2. **Completeness**: the cache key must include everything that can affect the output. Undeclared inputs break this; sandboxing prevents undeclared inputs.
3. **Value identity**: two values are "the same" if their content-addressed identity matches. For blobs: BLAKE3 hash. For trees: Merkle root. For typed compound values: structural hash.
4. **Early cutoff correctness**: if a computation reruns and produces a value with the same identity, its dependents must not be invalidated. This means the runtime must compare output identity before and after re-evaluation.

---

## 5. Output and artifact model

### 5.1 Blobs

A blob is immutable, pathless, content-addressed bytes. Identity is BLAKE3 hash. Large blobs are split via content-defined chunking; the store deduplicates shared chunks.

Blobs are the atoms of the store. Everything else — trees, artifacts, typed outputs — is built from blobs.

### 5.2 Trees

A tree is a content-addressed manifest of named entries. Each entry has a name (path component), a kind (file or directory), and a content reference (blob hash for files, tree manifest hash for subdirectories).

Trees are first-class values in the language, not just store implementation details:

- `collect_tree()` on a stream assembles a tree. Children at every level are lexicographically ordered. This is deterministic by construction.
- Trees can be merged, filtered, and have subtrees extracted.
- Trees are the natural input/output unit for `exec` sandboxes.
- Tree identity is the Merkle root hash.

### 5.3 Streams

A `Stream<T>` is an unordered, reusable, progressively-produced collection of typed elements.

**Unordered**: the language does not preserve, expose, or rely on producer order. Completion order is also not a semantic concept. Determinism comes from collection points, not from processing order.

**Reusable**: a stream is a value. Multiple downstream consumers observe the same elements. The producer runs once; the runtime caches produced elements.

**Progressive**: elements arrive as the producer yields them. Downstream `map` and `filter` operations process elements as they arrive, without waiting for the full collection. This enables `acquire` → `filter` → `map(compile)` to start compilations as archive entries are extracted.

**Collection boundaries**: operations that need the full collection force the stream. `collect_tree()`, `collect_list(order_by: ...)`, and passing all elements to a single downstream computation (e.g., a linker) are collection boundaries. `collect_list()` with no ordering specified is an error — there are no implicit ordering defaults.

Operations that do not exist on streams in v0: `zip` (requires ordering), `fold`/`reduce` (collection boundary in disguise), `group_by`/`join` (deferred until a real use case forces them).

### 5.4 Typed outputs from `exec`

`exec` returns a typed record derived from its output declarations:

```
result = exec(
  argv: [rustc, "--edition", "2024", "-o", "libfoo.rlib", ...],
  inputs: { src_tree, dep_rlibs },
  outputs: {
    rlib: file("libfoo.rlib"),
    depinfo: file("libfoo.d"),
    metadata: file("libfoo.rmeta"),
  },
)
// result.rlib: Blob
// result.depinfo: Blob
// result.metadata: Blob
```

For tree outputs (when the exact set of output files is not known statically):

```
result = exec(
  argv: [code_generator, "--out-dir", "gen/"],
  inputs: { grammar_files },
  outputs: {
    generated: tree("gen/"),
  },
)
// result.generated: Tree
```

The runtime captures the full tree under `gen/` after successful execution. The tree is content-addressed as a Merkle manifest. The declared output shape (`file` vs `tree`) tells the runtime what to capture.

### 5.5 Why generic `exec` is not a semantic stream

Consider a C compiler invocation:

```
exec(argv: [cc, "-c", "foo.c", "-o", "foo.o"], ...)
```

During execution, the compiler may:
1. Write `foo.o.tmp` (partial object file)
2. Write `foo.o` (final object file)
3. Delete `foo.o.tmp`

If the runtime streamed filesystem events as semantic values, consumers would see a temp file appear and then disappear. That is operational noise, not a semantic result.

The semantic result is: after successful exit, `foo.o` exists at the declared output path. That is the only thing the language should see.

Contrast with `acquire` (archive extraction):

```
acquire(url: "https://...", sha256: "...")
```

As the archive is extracted, each entry is individually final. Entry `bin/rustc` does not get overwritten by a later entry. The stream is **monotonic**: later elements do not invalidate earlier ones. This is why `acquire` can yield a semantic `Stream<TreeEntry>` while `exec` cannot.

The monotonicity criterion is the bright line: **a primitive may yield semantic stream elements only if each element is individually final and cannot be invalidated by subsequent execution.**

---

## 6. Store / GC / liveness model

### 6.1 Three states of a produced object

| State | Meaning | Lifetime |
|-------|---------|----------|
| **Observed** | The executor has seen output appear in the sandbox. | Ephemeral — exists only in executor process memory. |
| **Stored** | Bytes have been uploaded to CAS. A lease holds them alive. | Survives executor process, but not lease expiry. |
| **Published** | A memo table entry or demand result references the content-addressed object. | Survives GC as long as reachable from a root. |

### 6.2 Leases

When an `exec` begins, the runtime acquires a lease. All blobs uploaded to CAS during that execution are held by the lease. The lease prevents GC from reclaiming these objects even though no memo entry references them yet.

If the execution succeeds:
- Declared outputs are captured and content-addressed.
- The memo table entry for this computation is written, referencing the output hashes.
- The lease is released. The objects are now reachable from the memo entry (a root).

If the execution fails or the executor crashes:
- The lease expires (either explicitly or via timeout).
- GC may reclaim the unrooted blobs.
- No memo entry is written. The computation will be retried on next demand.

### 6.3 Roots and reachability

A **root** is a durable reference to a content-addressed object. Roots come from:

- Memo table entries: `compute_key -> result_hash` mappings. The stored result is reachable.
- Demand results: if the user demanded `my_binary` and it was realized, the result is a root until the demand is released or the memo entry is evicted.
- Pinned objects: explicitly pinned by the user or tooling (e.g., "keep this toolchain available").

GC walks reachability from all roots. Anything reachable (transitively, through Merkle manifests) survives. Everything else is eligible for collection.

### 6.4 Worked example: downloading and extracting a Rust toolchain

```
fn rust_toolchain(spec: ToolchainSpec) -> Tree {
  acquire(url: spec.url, sha256: spec.sha256)
    .collect_tree()
}

tc_tree = rust_toolchain(RUST_1_80_SPEC)
```

Execution proceeds as:

1. `rust_toolchain(RUST_1_80_SPEC)` is demanded. Orchestrator checks memo table: `memo[rust_toolchain, RUST_1_80_SPEC]`. Cache miss.

2. `acquire(url, sha256)` begins. A lease is acquired.

3. The runtime downloads the archive, verifying the hash as bytes arrive. Extraction begins immediately (streaming).

4. As each archive entry is extracted, its content is uploaded to CAS under the lease. The stream yields `TreeEntry` elements to the `collect_tree()` operation.

5. `collect_tree()` assembles entries into a Merkle tree manifest as they arrive. When the archive is exhausted, the final manifest is written to CAS.

6. The memo table entry is written: `memo[rust_toolchain, RUST_1_80_SPEC] -> tree_manifest_hash`.

7. The lease is released. All blobs and the manifest are now reachable from the memo entry.

8. On next demand for the same toolchain spec, step 1 finds a cache hit. The tree manifest is already in the store. No download occurs.

If the download fails at step 3 (network error): the lease expires, partially-uploaded blobs are GC-eligible, no memo entry is written, and the next demand retries the download. This is a transient failure — it is not cached.

If the hash doesn't match at step 3: this is a permanent failure. The runtime reports it. Whether to cache this failure is a policy question — the URL may have been compromised, or the spec may be wrong. Conservatively: do not cache hash-mismatch failures.

---

## 7. Incrementality and invalidation

### 7.1 What triggers re-evaluation

| Change | What invalidates |
|--------|-----------------|
| **Source file content changes** | Any `read` of that file invalidates. Downstream computations re-evaluate. Early cutoff may stop propagation (e.g., reformatted Cargo.toml). |
| **Source file added/removed** | Any `glob` or `children` observation that covers the affected directory invalidates. Downstream recomputation discovers new or missing entries. |
| **Toolchain changes** | Toolchain identity is part of `exec` cache keys (it's an input). All compilations using that toolchain invalidate. |
| **Vixen function changes** | The function's AST hash changes, so any memo entry for that function invalidates. Transitive effects are handled by early cutoff: if the function produces the same result despite its code changing, downstream is unaffected. |
| **Runtime semantics change** | The runtime version stamp is part of cache keys. A semantics change invalidates everything. This should be rare. |

### 7.2 File watching vs. authoritative checks

The orchestrator may run as a daemon (`vxd`) and use file watching (inotify, FSEvents, ReadDirectoryChangesW) to detect source changes.

File watch events are **hints, not authoritative**. When the daemon receives a file change notification:

1. It marks potentially-affected memo entries as "suspected stale."
2. On next demand, it re-reads the affected files and computes their BLAKE3 hashes.
3. If the hash matches the previously recorded hash: the suspicion was false (or the file was modified and then reverted). No invalidation.
4. If the hash differs: the memo entry is invalidated and dependent computations re-evaluate.

This means correctness never depends on file watching. If file watching is unavailable (remote build, CI, no daemon), the system falls back to hashing all inputs on demand. File watching only improves latency.

### 7.3 Early cutoff in detail

Early cutoff is checked at every tracked computation boundary:

1. A computation's inputs have changed (transitively, some upstream dependency produced a different value).
2. The runtime re-evaluates the computation.
3. The runtime compares the new output's content-addressed identity to the previous output's identity.
4. If identical: dependents are NOT re-evaluated. The change stops propagating.
5. If different: dependents are marked for re-evaluation. The process continues.

This is the mechanism that prevents a Cargo.toml reformatting from triggering a full rebuild. It is also the mechanism that allows a toolchain update to avoid rebuilding crates whose compilation output happens to be identical (uncommon but possible with no-op updates).

### 7.4 Concrete example: Rust crate with dynamically discovered tests

```
fn build_and_test(pkg: RustPackage, tc: RustToolchain) -> TestResult {
  lib = build_lib(pkg, tc)

  // Build the test binary — this is a separate compilation
  test_bin = exec(
    argv: [tc.rustc, "--test", ...],
    inputs: { pkg.srcs, lib.metadata, deps... },
    outputs: { binary: file("test_binary") },
  ).binary

  // Run the test binary to discover and execute tests
  exec(
    argv: [test_bin, "--format", "json"],
    inputs: { test_bin },
    capture_stdout: true,
    outputs: {},
  ) |> parse_test_results
}
```

Dynamic test discovery is natural: the test binary itself determines which tests exist. This is not a dependency graph trick; it is ordinary tracked evaluation. If the source changes, the test binary changes, which changes the test results. If the source changes in a way that does not affect the test binary (early cutoff), the tests are not re-run.

---

## 8. Failure, cancellation, and crash recovery

### 8.1 Executor dies mid-build

If an executor process dies while running an `exec`:
- The lease on any uploaded blobs expires. GC may reclaim them.
- No memo entry is written.
- The orchestrator detects the lost connection and may retry on another executor (if available) or report the failure.
- No partial or corrupt results enter the memo table. The system is in the same state as if the execution never started.

### 8.2 Partial uploads

If the executor uploaded some output blobs to CAS but crashed before uploading all of them:
- The uploaded blobs are held by the lease. They are valid content-addressed objects — there is no corruption risk from partial upload (each blob is individually verified by BLAKE3).
- When the lease expires, the unreferenced blobs are GC-eligible.
- No data is lost that wasn't already in the store.

### 8.3 Daemon absent

If `vxd` is not running and cannot be started:
- `vx` (the CLI) reports the error and exits.
- No fallback "run everything locally without the daemon" mode. The daemon is the orchestrator. Without it, there is no build.
- In bootstrap mode (no `VXD` env var), `vx` will attempt to spawn `vxd`. If that fails, it's a hard error.

### 8.4 Build interrupted (Ctrl-C)

If the user interrupts a build:
- The orchestrator cancels in-flight `exec` requests.
- Executors terminate running processes.
- Leases expire. Partially-produced objects are GC-eligible.
- Completed and memo'd computations remain valid in the cache. The next build starts from where the last successful memo entries left off.
- No half-written memo entries: the memo table uses atomic writes.

### 8.5 Correctness guarantees

1. **The memo table never contains invalid entries.** An entry is written only after successful execution and output capture. Atomic writes prevent partial entries.
2. **The store never contains corrupt objects.** Every blob is verified by BLAKE3 on write and on read. A bit flip is detected, not silently served.
3. **An interrupted build cannot leave the system in a state where the next build produces wrong results.** Leases and atomic memo writes ensure this.
4. **Transient failures (network, OOM) do not produce cached failure entries for `download`/`acquire`.** Only deterministic failures may be cached, and only for primitives where the failure is determined by the inputs.

---

## 9. Implementation plan

### 9.1 What already exists

The following infrastructure is already implemented:

- Content-addressed store (`vx-store`) with BLAKE3, CDC chunking, Merkle manifests
- Executor with sandboxed VFS (`vx-runner`, `vx-vfsd`)
- VFS backends: FSKit (macOS), FUSE (Linux), ProjFS (Windows)
- Orchestrator daemon (`vxd`) with RPC via roam
- Daemon lifecycle, multiplexing, and bootstrap (`vx`)
- Rust compilation pipeline (hardcoded, not yet driven by the language) (`vx-rs`)
- Tarball acquisition (`vx-tarball`)
- Toolchain management (`vx-toolchain`)

### 9.2 Staged plan

#### Stage 0: Language kernel (must have for correctness)

The irreducible foundation. Without these, nothing else makes sense.

1. **Parser and CST.** Lossless concrete syntax tree. Expression-oriented. Typed bindings, function definitions, pattern matching, collection literals. No need for the full type system yet — type checking can come later if evaluation is dynamically typed initially.

2. **Evaluator with memoization.** Demand-driven evaluation of Vixen expressions. Memoize tracked computations by (function identity + argument identity). Detect cycles at runtime. Support early cutoff (compare output identity before/after).

3. **`exec` integration.** Wire the evaluator to the existing `vx-runner`. Structured inputs (trees + placed files), structured outputs (declared files + trees), sandboxed execution, output capture to store.

4. **`acquire` as a stream.** Wire to the existing `vx-tarball`. Produce `Stream<TreeEntry>`. Support `filter`, `map`, `collect_tree()`.

5. **Workspace root as a Tree.** The runtime provides the workspace root as a Tree handle. `entry()`, `children()`, `glob()` work over it. `read_bytes()` / `read_text()` on entries. All tracked.

6. **One end-to-end example.** Compile a single Rust crate using Vixen source that calls `exec` with `rustc`. Inputs from workspace tree. Toolchain from `acquire`. Output captured to store. Demonstrate memoization and early cutoff.

**This stage replaces the hardcoded `vx-rs` pipeline with one driven by Vixen source.**

#### Stage 1: Standard library bootstrap (must have for usability)

7. **Rust compilation rules in Vixen.** `build_lib`, `build_bin`, `build_test_bin`, `run_tests`. Handle `deps`, `edition`, `features`. Parse `Cargo.toml` for metadata (using `parse_toml`).

8. **Multi-crate workspace.** Recursive dependency resolution. Demonstrate memoization across crates.

9. **Toolchain acquisition in Vixen.** `acquire` + `collect_tree` + `toolchain_from_distribution`. Replace hardcoded `vx-toolchain` logic.

10. **Proc macro support.** Proc macros are host-target executables compiled during the build. This is a natural test of dynamic dependencies.

#### Stage 2: Robustness (must have for production)

11. **Lease and GC implementation.** Leases on in-flight blobs. Root tracking from memo table. GC sweep of unreachable objects.

12. **Failure caching.** Primitive-specific: cache hermetic failures, don't cache transient failures.

13. **Daemon file watching.** File change hints → suspected-stale → authoritative hash check on demand.

14. **Concurrent scheduling.** Orchestrator dispatches independent `exec` calls concurrently. Backpressure / concurrency limits.

#### Stage 3: Distribution (can come later)

15. **Remote store.** CAS accessible over the network. Orchestrator and executors can be in different locations.

16. **Remote execution.** Dispatch `exec` to remote runners. Inputs/outputs transit via store, not direct transfer.

17. **Shared cache.** Multiple developers / CI machines share a store. Memo entries are portable (cache keys don't include machine-specific state).

#### Stage 4: Language maturity (can come later)

18. **Type checking.** Static type system. Type errors before evaluation. Typed function signatures.

19. **LSP and editor support.** Completions, diagnostics, go-to-definition. Built on the lossless CST.

20. **C/C++ rules.** Standard library support for C and C++ compilation, including resolved compile/link contexts.

21. **OCI image assembly.** Layer construction, image manifests. Proof that the model generalizes beyond compiler pipelines.

22. **Cross-compilation.** Host/target platform distinction. Toolchain selection by target.

---

## 10. Open questions and risks

### 10.1 Unresolved design questions

**Function identity under code changes.** Hashing the normalized AST is the proposed approach, but the details matter. How are closures handled? If a function captures a value and the captured value's identity is part of the function's identity, do we hash the value or a reference to the computation that produced it? This needs a concrete specification before Stage 0 can ship.

**Stream backpressure.** Streams are reusable and cached. But if a stream produces 100,000 elements and only one consumer needs 10, does the runtime eagerly cache all 100,000? Or can it stop the producer? Reusability implies "the producer runs to completion" since another consumer might need all elements. But that could be wasteful. Possible resolution: reference counting on stream consumers, with the producer stopping when all consumers have detached.

**Tree mutability during collection.** `collect_tree()` assembles a tree from a stream. But what if two stream elements have the same path? Last-write-wins? Error? This needs a deterministic resolution policy.

**Late-bound inputs.** "Current Rust stable toolchain" resolves to a concrete version. But when does that resolution happen? At demand time? At module load time? If the resolution is cached, when does the cache expire? This is a policy question that interacts with cache invalidation in non-obvious ways.

**Error aggregation.** We said fail-fast with deterministic tie-breaking. But the UX question remains: should `vixen test` behave differently from `vixen build`? If so, is that a language-level distinction or a CLI-level presentation choice? Current answer: CLI-level. But that means the runtime needs to support both "stop after first failure" and "continue and aggregate" modes.

**What types are "demandable."** We said tooling distinguishes ordinary values from realizable build products by type, not by visibility. But who decides which types are demandable? Is `Tree` demandable? `Blob`? Only `Executable`, `RustLibrary`, etc.? This is a tooling convention, not a language rule, but it needs to be defined.

### 10.2 Risks

**Complexity of the evaluator.** A demand-driven, memoizing evaluator with dynamic dependencies, stream support, early cutoff, and cycle detection is not trivial to build correctly. The interaction between streams, memoization, and early cutoff is particularly subtle. **Mitigation**: Stage 0 can defer streams and handle only concrete values and `exec`. Streams can be added in Stage 1.

**Cache explosion.** If every intermediate computation is memoized, the store can grow very large. Especially with Merkle manifests for trees, where small changes produce new manifests. **Mitigation**: GC with reachability from roots. Eviction policies (LRU on memo entries). CDC chunking for deduplication. But the eviction policy needs design work.

**Sandbox escape.** The correctness of caching depends entirely on sandbox correctness. If a process can read undeclared inputs (e.g., system headers, locale settings, /dev/urandom), cache keys are incomplete and results may be non-reproducible. **Mitigation**: VFS enforcement (process can only see declared inputs). Platform sandboxing (Landlock, sandbox profiles). But no sandbox is perfect — this is an ongoing arms race.

**Language design risk.** The language does not exist yet. The runtime semantics are well-defined, but the surface syntax and type system are not. There is a risk that the language design process reveals incompatibilities with the runtime model, or that the language is too austere for practical build rules. **Mitigation**: Write realistic build rules in pseudo-Vixen before finalizing the language. Stage 0 can use a minimal syntax and iterate.

**Performance of content-addressed everything.** BLAKE3 is fast, but hashing every file, every tree, every intermediate value adds overhead. For large monorepos with millions of files, this could be significant. **Mitigation**: BLAKE3 is explicitly designed for this (SIMD, parallelism). CDC chunking amortizes rehashing for similar files. But empirical validation is needed.

### 10.3 Where the design might collapse under its own ambition

**Streams + memoization + early cutoff.** The interaction between these three features is the most complex part of the design. A stream is a progressively-produced value. Memoization caches it. Early cutoff compares it to the previous run. But what does it mean to compare two streams for equality? If the stream is unordered, do you compare element sets? What if the stream is very large? This triple interaction needs a formal specification, not just handwaving about "the runtime handles it."

**Narrower version that preserves the essence**: defer streams to Stage 1. In Stage 0, `acquire` returns a `Tree` (blocking), not a `Stream<TreeEntry>`. All values are concrete. Memoization and early cutoff work on concrete values, which is well-understood. Add streams once the evaluator is stable and the concrete model is validated.

**Remote execution + streaming.** If `acquire` produces a stream and a remote executor needs the resulting tree, the stream must be collected somewhere before it can be referenced by Merkle root. Where? On the orchestrator? On the store directly? If the acquire is itself remote, the stream elements are already in the remote store — but the orchestrator needs to know the final tree root to dispatch dependent work. This interaction is tractable but needs careful design.

**Narrower version**: in Stage 0-2, all execution is local. The store is local. Remote execution is Stage 3. This avoids the distributed-streaming problem entirely until the local model is proven.

---

## Architectural invariants

These must hold for the system to be correct. Violations are bugs.

1. **Determinism**: given the same source files, toolchains, and runtime semantics version, the system produces the same outputs regardless of scheduler order, parallelism level, or executor location.

2. **Sandbox completeness**: an `exec` computation can only observe its declared inputs, declared environment, and the executable specified in argv. No undeclared host state leaks in.

3. **Cache soundness**: a memo table entry is written only when the computation has successfully completed and its outputs are durably stored. No partial or speculative memo entries.

4. **Content-addressed integrity**: every object in the store is verified by BLAKE3 on write and on read. Bit flips are detected, not silently served.

5. **Early cutoff correctness**: if a computation re-evaluates and produces a result with the same content-addressed identity, its dependents are not re-evaluated.

6. **Lease safety**: in-flight blobs are held by leases. GC never reclaims a leased object. Leases expire only on successful commit, explicit cancellation, or timeout.

7. **No semantic dependence on runtime telemetry**: stderr, timings, progress events, CAS upload notifications, and scheduler decisions never affect computation results or cache keys.

8. **Monotonic stream safety**: a primitive may yield semantic stream elements only if each element is individually final and later elements cannot invalidate earlier ones.

9. **Demand purity**: the language has no construct that triggers evaluation. All demand originates from outside the language boundary.

10. **Store is the rendezvous**: orchestrator and executor never exchange file contents directly. All data flows through the content-addressed store.

---

## Unresolved questions (pre-implementation)

1. How exactly is function identity computed? What is the normalized AST hash? How are closures and captured values handled?

2. What is the concrete eviction/GC policy for memo entries? LRU? Reference counting? Manual pinning? Time-based expiry?

3. What is the resolution and caching policy for late-bound inputs like "current Rust stable"?

4. What happens when two stream elements in `collect_tree()` have the same path?

5. What is the concrete representation of a typed compound value's content-addressed identity? Structural hash of all fields? Merkle-style? What about types with no meaningful structural identity (e.g., opaque host handles)?

6. How is the runtime semantics version stamp managed? Is it a hash of the evaluator binary? A manually-bumped version? What granularity of change triggers full invalidation?

7. What is the backpressure / early-termination model for streams with consumers that don't need all elements?

8. Should the language support user-defined types, or are all types built-in / provided by the standard library?

9. What is the error model for `exec` in detail? Is the error value just "failed with exit code N and stderr"? Or is there richer structure?

10. How does the system handle non-deterministic tools (compilers that embed timestamps, tools that use randomized algorithms)? Is this purely a documentation/policy concern, or does the runtime attempt to detect it?

---

## Proposed MVP cut

**The MVP is Stage 0**: language kernel + evaluator + `exec` integration + workspace tree + one end-to-end Rust crate compilation.

Specifically included:
- Parser for a minimal Vixen subset (bindings, function definitions, function calls, string literals, list literals, record literals, field access)
- Demand-driven evaluator with memoization and early cutoff
- `exec` wired to existing `vx-runner`
- `acquire` returning a `Tree` (not a stream — deferred)
- `read_bytes`, `parse_toml` for manifest parsing
- Workspace root as a Tree handle with `entry()` and `glob()`
- One working example: compile a single Rust crate from Vixen source

Specifically excluded from MVP:
- Streams (all collections are concrete)
- Type checking (evaluation is dynamically typed)
- Remote execution (all local)
- GC / leases (store grows unbounded in MVP)
- File watching (manual rebuild trigger)
- Multi-crate workspaces (single crate only)
- C/C++ rules
- LSP / editor support

The MVP proves: **the evaluator can drive real compilation through `exec`, memoization works, early cutoff works, and the architecture holds together end-to-end.** Everything else builds on that foundation.
