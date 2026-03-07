# Vixen Design Spec

## 1. Motivation and Framing

### 1.1 Problem statement

Vixen is intended to solve a class of build and artifact-construction problems
 that existing systems handle only awkwardly:

- build logic is naturally program-shaped, not rule-table-shaped
- dependencies are often discovered during evaluation rather than known in a
  separate declaration phase
- correctness should not depend on the incidental order in which work was
  scheduled
- reuse should happen at the level of semantic values, not only at the level of
  "did we already run this command"
- remote/shared execution should not force a separate mental model from local
  execution

The target system is therefore not "a nicer task runner". It is a deterministic
 distributed evaluator specialized for build-shaped effects.

The core bet is:

> Treat build definitions as a pure-ish language over typed values, and model
> external execution as a small set of tracked effect primitives. Let the
> runtime discover, memoize, and schedule the resulting computation graph.

That is a stronger claim than "dynamic build graph". It says the graph is a
 runtime artifact of evaluation, not the primary authoring model.

### 1.2 Contrast with existing systems

The point is not that existing systems are bad. The point is that they optimize
 for different semantic centers.

| System family | Primary unit | Dependency discovery | Caching center | Where Vixen differs |
| --- | --- | --- | --- | --- |
| Make-like | file + command | mostly static, file-based | timestamps / file state | Vixen is content-based, value-based, and does not center source semantics on files |
| Bazel-like | declared action / target | configured graph, largely explicit | action cache + artifact graph | Vixen does not require a strict rule-definition phase before evaluation |
| Shake-like | tasks with dynamic deps | dynamic during build | task result cache | Vixen goes further toward a language evaluator with first-class pure computations |
| Nix-like | derivation / store object | mostly explicit, pure-ish construction | content-addressed store | Vixen makes dynamic dependency discovery and demand-driven evaluation first-class |

Make is fundamentally about propagating file changes through commands. Bazel is
 fundamentally about configured targets and declared actions. Shake is about
 dynamic dependencies but still treats tasks as the authoring center. Nix is
 about pure derivation descriptions and store objects.

Vixen is closer to "distributed memoized evaluation with sandboxed effects"
 than to any one of those systems, although it borrows heavily from all of
 them.

### 1.3 Build Systems à la Carte framing

Build Systems à la Carte is the right framing device because it separates a
 build system into:

- tasks
- a scheduler
- a rebuilder

That decomposition is useful here, but only up to a point.

If forced into that taxonomy, Vixen looks roughly like this:

- **Tasks**: not user-authored rules, but runtime-discovered effectful and pure
  computations, especially `exec`, `acquire`, file/tree observations, and pure
  memoized functions
- **Scheduler**: demand-driven and opportunistic rather than "run a known DAG in
  topological order"
- **Rebuilder**: content-addressed and value-based, with semantic equality and
  early cutoff rather than only action replay prevention

That already stretches the paper's taxonomy. The places where it becomes
 insufficient are exactly the places where Vixen stops looking like a classical
 build system:

- the primary authoring unit is not a task but a value definition
- the runtime graph contains both pure and effectful nodes
- dependencies are discovered by ordinary evaluation, not only by task
  declarations
- semantic streams exist for monotonic producers
- publication, leases, and durable roots matter for correctness and GC
- memoization is over typed computations, not just external actions

So Build Systems à la Carte still helps frame the design, but Vixen extends it
 in three directions:

1. **Task formation is dynamic and evaluation-driven**
2. **Pure computations are first-class cacheable graph nodes**
3. **Runtime publication/storage policy is part of the architecture**

## Non-goals

This design does **not** aim to be:

- a generic distributed compute platform for arbitrary side effects
- a shell-oriented task runner
- a system where undeclared host state may influence results
- a source language for observing provisional filesystem mutations
- a fully general stream algebra in v0
- a static-DAG planner with a prettier front end

The intended scope is narrower and harsher:

- deterministic build-shaped effects
- explicit inputs and controlled environments
- demand-driven evaluation
- value-based caching and semantic reuse

## 2. Core Semantic Model

### 2.1 Modules, values, and demand roots

Vixen source defines:

- modules
- types
- values
- functions

Demand is external to the language.

That means:

- no `force`
- no default target in source
- no top-level side-effecting statements
- no separate "rule-definition phase" visible to authors

A Vixen file is a module namespace. The CLI or API demands one or more exported
 top-level values from that namespace.

Example:

```text
toolchain = resolve_rust_toolchain(channel: Stable)
crate = build_crate(workspace, toolchain)
tests = discover_tests(workspace, toolchain)
image = build_image(crate, assets)
```

None of these declarations performs work eagerly. They define typed values whose
 realization is driven from outside:

```text
vixen build crate
vixen test tests
vixen build image
```

### 2.2 Typed values and memoized computations

The primary semantic object is a typed value. Values are not divided in the
 surface syntax into:

- "pure values"
- "tracked values"
- "future values"

There is no `Tracked<T>`/`Future<T>` coloring in the surface language.

Instead, the runtime internally distinguishes:

- pure computations
- tracked observations
- tracked effect primitives

The language-level claim is still:

> A binding denotes a typed value in a computation graph.

That graph is memoized. Reuse is therefore not limited to artifacts produced by
 external commands; it also applies to intermediate pure computations and typed
 decoded results.

### 2.3 Dynamic dependencies

Dependencies are discovered during evaluation.

This is not an "escape hatch". It is the normal model.

Examples:

- parsing a manifest and discovering which files to read next
- resolving `Stable` to a concrete Rust version
- discovering tests under a tree during evaluation
- collecting generated outputs into a tree and then compiling them

There is no strict semantic split between:

- rule definition
- graph generation
- execution

The runtime graph emerges from evaluating demanded values.

### 2.4 Demand-driven evaluation

Evaluation is demand-driven:

- computations only start when some demanded value depends on them
- independent computations may start as soon as their inputs are ready
- computations that are no longer needed may be cancelled or deprioritized

Correctness must be independent of scheduler order, except where the language
 explicitly imposes a deterministic order at collection/publication boundaries.

### 2.5 Determinism assumptions

The system assumes:

- pure functions are deterministic over their arguments
- tracked host primitives are deterministic with respect to their declared
  inputs and execution environment
- undeclared inputs are not observable

This is a design requirement, not a best-effort hope. If a primitive or action
 can observe undeclared state, memoization becomes unsound.

### 2.6 Semantic status of `exec`

`exec` is semantically a deterministic tracked computation:

```text
exec(spec) -> TypedOutputs
```

where `spec` includes declared inputs, declared environment, declared outputs,
 and execution policy.

Semantically, `exec` is treated like a function:

- its result can be memoized
- its result can be reused remotely
- it participates in value-based invalidation

Operationally, `exec` is not "just another pure reduction". It is a privileged
 effect primitive that must be scheduled, sandboxed, mounted, and captured.

This spec adopts the following position:

- **Model A semantically**: `exec` behaves like a function from declared inputs
  to typed outputs
- **Model B operationally**: the runtime treats `exec` as orchestrated work with
  resource limits, leases, and publication

### 2.7 Semantic values vs operational telemetry

A core line must stay sharp:

- semantic values are part of language meaning and memoized results
- operational telemetry is runtime-internal or observability-facing

Examples of semantic values:

- a parsed manifest AST
- a `RustLibrary`
- a `Tree`
- captured stdout bytes when explicitly declared as part of the result

Examples of telemetry:

- stderr logs for a successful process
- timings
- mount statistics
- scheduler queue states
- provisional output observations during generic `exec`

Generic `exec` must **not** expose provisional filesystem mutations as semantic
 values.

## 3. Operational Model

### 3.1 Components

The runtime is composed of:

- **Orchestrator**: evaluates Vixen near source, maintains memo tables,
  resolves demand roots, and dispatches effectful work
- **Store**: content-addressed storage for blobs, trees, and published values
- **Executors**: run sandboxed `exec` computations locally or remotely
- **VFS projection layer**: mounts store-backed content into execution sandboxes

### 3.2 Control flow

The intended operational flow is:

1. the user demands a top-level value
2. the orchestrator evaluates the necessary pure and tracked computations
3. when a host primitive is needed, the orchestrator either performs it locally
   or schedules it
4. for `exec`, the executor receives a fully concrete execution spec
5. inputs are projected from the store into the sandbox through the VFS
6. outputs are captured, stored, and finally published if the computation
   succeeds
7. downstream computations consume published values by handle

### 3.3 Opportunistic scheduling

Scheduling is opportunistic:

- a computation may start as soon as its inputs are ready
- the runtime is free to overlap independent work
- the scheduler is not allowed to change the semantic result

This is why ordering must not leak accidentally from execution order into
 language meaning.

### 3.4 Local and remote execution

Local and remote execution should have the same semantics.

The differences are operational:

- remote execution requires content availability and transport
- local execution may short-circuit some transport
- both must obey the same sandbox/VFS/input declaration rules

If local execution can see state that remote execution cannot, the model is
 broken.

### 3.5 Sandboxing and VFS projection

Sandboxing is mandatory for soundness.

An `exec` spec must declare:

- argv
- env
- mounted tree/file inputs
- declared outputs

Tasks must not be able to:

- read undeclared files
- access arbitrary network
- observe ambient host state outside the declared environment

The VFS projection layer is not an implementation detail that can be skipped.
It is how the runtime makes content-addressed values appear as process inputs
 without bulk copying everything into an executor first.

## 4. Cache and Identity Model

### 4.1 Why this is the hard part

This system only works if computation identity is stable and sound.

That is the real difficulty. If identity is underspecified, the cache lies. If
 identity is overspecified, reuse collapses.

### 4.2 Distinct kinds of identity

The design needs to distinguish at least these identities:

- **Blob identity**: digest of immutable bytes
- **Tree identity**: digest of canonicalized tree structure plus child
  identities
- **Value identity**: structural identity/equality of typed values
- **Function identity**: stable identity of function semantics
- **Computation identity**: function identity plus argument identities plus all
  relevant declared context
- **Execution identity**: the identity of one concrete `exec` spec

These are related, but they are not the same.

### 4.3 What must go into computation identity

At minimum, a memoized computation key must include:

- function identity
- argument identities
- resolved late-bound values
- relevant runtime/language semantics version
- relevant host primitive contract version

For `exec`, the identity must include:

- argv
- env
- declared input tree/file identities
- declared output schema
- sandbox policy
- toolchain identity
- platform/executor contract version

If any of those are omitted, the cache may return a result computed under
 meaningfully different conditions.

### 4.4 Late-bound inputs

Late-bound inputs are allowed, but they must resolve to concrete values that
 participate in identity.

Example:

```text
stable = resolve_rust_channel(channel: Stable)
toolchain = fetch_toolchain(stable)
```

`Stable` itself is not a good cache key. The resolved concrete version is.

That means:

- the resolver is a memoized computation
- its result is a concrete version/value
- downstream computations key on the resolved value, not on the alias alone

### 4.5 Function identity

This is a likely failure point.

The design must answer:

- what is the stable identity of a user function
- how code changes invalidate previous memo cells
- how standard library changes invalidate previous memo cells
- how closures/captured environments, if allowed, are represented

Opinionated recommendation for a survivable first system:

- memoize only top-level named functions and host primitives initially
- give each compiled function/HIR body a stable semantic hash
- treat code/stdlib/runtime semantic version as part of the key
- defer exotic closure identity rules until there is a real need

Anything more ambitious risks collapsing under key instability.

### 4.6 Value-based caching

Value-based caching is not mystical. It means:

- computations are memoized by computation identity
- a rerun produces a new candidate result value
- the runtime compares that value to the previously published value
- if they are semantically equal, downstream recomputation is skipped

This requires each cached type to have a meaningful equality notion.

Examples:

- blobs/trees: digest equality
- parsed ASTs: structural equality
- typed records: fieldwise structural equality
- opaque executor handles: should not exist as semantic values unless their
  equality is well-defined

If a type has no stable equality, it is a bad semantic cache boundary.

## 5. Output and Artifact Model

### 5.1 Core runtime objects

The runtime should distinguish the following concepts:

- **Blob**: immutable content in the store, identified by digest
- **Artifact**: a typed reference to stored content plus semantic metadata
- **Tree**: a structured collection of named artifacts/subtrees
- **TreeEntry**: one path-bearing entry discovered within a tree-producing
  operation
- **Stream<T>**: a progressive, reusable, unordered collection of typed elements
- **TypedOutputs**: the committed typed result record of an `exec`

The design should not collapse these into "everything is a file" or "everything
 is a generic blob".

### 5.2 File and tree values

Pathless file content is the primitive. Placement is contextual.

That means:

- `write_file(content)` produces pathless file content
- the same content may be placed at multiple paths without becoming a different
  primitive object
- paths are tree-relative placement metadata, not ambient host identities

Trees are first-class values, not just `Map<Path, Blob>`.

They carry structural semantics:

- membership
- placement
- subtree identity
- merge/filter/subtree operations

### 5.3 Streams

`Stream<T>` exists only when progressive production is semantically meaningful.

Current design:

- streams are reusable, not linear
- streams are unordered in the language semantics
- `map`/`filter` preserve progressiveness
- `zip`, `fold`, `reduce`, `group_by`, and `join` are not part of v0 stream
  semantics
- structural observations such as `children(tree)` and `glob(tree, pattern)`
  produce `Stream<TreeEntry>`

Collection points impose deterministic structure:

- `collect_tree()` canonicalizes by path/tree structure
- `collect_list(order: ...)` requires an explicit ordering policy

### 5.4 Monotonic producers vs generic `exec`

This is the decisive split.

Some primitives are monotonic:

- later yields do not invalidate earlier yielded values
- semantic streaming is safe

Example:

```text
toolchain_entries = acquire(url: toolchain_url, sha256: toolchain_hash)
```

As each archive member is verified and decoded, the runtime may yield:

```text
TreeEntry { path: "rustc/bin/rustc", content: ..., kind: File }
TreeEntry { path: "rustc/lib/std.rlib", content: ..., kind: File }
...
```

Later entries do not invalidate earlier ones. This is a real semantic stream.

Generic `exec` is different.

Example:

```text
exec(
  argv: [cc, "-c", "foo.c", "-o", "foo.o"],
  inputs: { src_tree },
  outputs: { obj: file("foo.o") },
)
```

During execution the process may:

- create `foo.o.tmp`
- rename `foo.o.tmp` to `foo.o`
- delete temp files
- rewrite outputs

Those mutations are not a semantic stream of values. They are operational
 behavior. The semantic result exists only at commit time, when the declared
 outputs are known and published.

This is why:

- `acquire` may return `Stream<TreeEntry>`
- generic `exec` returns final committed `TypedOutputs`

### 5.5 Dynamic output trees

The output shape may be declared without enumerating every file in advance.

Example:

```text
result = exec(
  argv: [generator, "--out", "out"],
  inputs: { schema_tree },
  outputs: { generated: tree("out") },
)
```

The runtime knows:

- there will be a tree rooted at `out`
- the exact contents are discovered during execution
- the committed semantic result is the final canonical `Tree`

This preserves a clear publication boundary without forcing static enumeration
 of every file.

### 5.6 Compiling a Rust crate with dynamically discovered tests

Example sketch:

```text
workspace = source_tree()
crate_manifest = parse_toml(read_text(workspace.entry("Cargo.toml")))
toolchain = resolve_rust_toolchain(channel: Stable)

lib = rust::compile_library(workspace, crate_manifest, toolchain)

tests = glob(workspace.entry("tests"), "**/*.rs")
  .map((entry) => rust::compile_test(workspace, entry, toolchain))
  .collect_list(order: path)

crate = RustPackage {
  lib: lib,
  tests: tests,
}
```

What matters here:

- tests are discovered dynamically during evaluation
- each test compilation may start as soon as its inputs are ready
- the final test list imposes explicit deterministic ordering

## 6. Store / GC / Liveness Model

### 6.1 Three states

The runtime/storage model should explicitly distinguish:

- **Observed**: an in-flight execution noticed an output candidate
- **Stored**: content bytes/tree pieces are present in CAS
- **Published**: a committed semantic result references the value

These states are not interchangeable.

### 6.2 Leases and roots

The store needs two liveness mechanisms:

- **Lease**: temporary liveness for in-flight work
- **Root**: durable reachability for committed results

While an `exec` or `acquire` is in progress:

- candidate outputs may be uploaded eagerly
- those stored objects live under a lease
- they are not yet semantic results

When the computation succeeds:

- the runtime assembles the committed semantic result
- a memo entry or other durable root references it
- the result becomes published and reachable for GC

If the computation fails or is abandoned:

- no semantic result is published
- leased objects may be reclaimed after lease expiry

### 6.3 Worked example: downloading and extracting a Rust toolchain

Consider:

```text
version = resolve_rust_channel(channel: Stable)
entries = acquire(url: version.url, sha256: version.sha256)
toolchain = entries.collect_tree()
```

Operationally:

1. `resolve_rust_channel(Stable)` produces a concrete version object
2. `acquire(...)` begins downloading and extracting the archive
3. decoded members are uploaded into CAS under a lease
4. `TreeEntry` values are yielded semantically as the archive progresses
5. `collect_tree()` consumes the stream and canonicalizes a final tree object
6. the memo entry for `resolve_rust_toolchain(Stable -> concrete_version)`
   points to that tree
7. that memo entry is the durable root

The important property is:

- the stream is semantically meaningful because archive extraction is monotonic
- the final tree is what becomes rooted and retained
- leased intermediate storage survives long enough for commit

### 6.4 GC

GC should operate over:

- durable roots
- memo table reachability
- explicit user pins/materializations
- active execution leases

Everything else is collectible.

Opinionated recommendation:

- use tracing/reachability GC for published objects
- use time-bounded leases for in-flight objects
- do not make long-term retention depend on whether some consumer happened to
  inspect a stream "in time"

If semantics depend on timing of stream consumption, the design is already
 broken.

## 7. Incrementality and Invalidation

### 7.1 Authoritative change detection

File watching and daemon notifications are hints only.

Authoritative change detection is content-based and semantic:

- file content identity
- tree identity
- resolved late-bound inputs
- computation identity
- semantic equality of results

mtime is not authoritative.

### 7.2 File change vs semantic change

Example: a Cargo manifest changes formatting but not meaning.

```text
manifest_text = read_text(workspace.entry("Cargo.toml"))
manifest_ast = parse_toml(manifest_text)
crate = plan_crate(manifest_ast)
```

If the file bytes change:

1. `read_text` invalidates
2. `parse_toml` reruns
3. if the parsed AST is structurally equal to the previous AST, `plan_crate`
   does not need to recompute

This is the point of value-level caching and early cutoff.

### 7.3 Toolchain changes

If "current stable" resolves to a new concrete version, that is a real semantic
 change.

It should invalidate computations that depend on the concrete resolved toolchain
 value, even if the source still says `Stable`.

### 7.4 Code and runtime changes

The following must invalidate memoized computations:

- changes to function bodies/compiled HIR
- changes to standard library functions used by the computation
- changes to host primitive semantics or ABI
- changes to runtime semantic policy that affect evaluation meaning

If this is not explicit in the identity model, old cached values will be reused
 under new semantics.

### 7.5 Early cutoff

Early cutoff requires:

- deterministic equality for cached value types
- reevaluation on changed upstream identities
- replacement of the new result with the previous published identity when equal

Without equality semantics, early cutoff is impossible.

## 8. Failure, Cancellation, and Crash Recovery

### 8.1 Failures

For one demand root:

- reporting is fail-fast
- the runtime does not wait for all independent branches to fail
- if multiple failures are already available when reporting happens, tie-breaking
  should be deterministic

Failure caching is primitive-specific:

- hermetic parse/validation/`exec` failures are cacheable
- transient network failures are not necessarily final cached results

### 8.2 Cancellation

Cancellation is operationally important but must not affect published semantics.

If work is no longer needed:

- the orchestrator may stop scheduling new dependent work
- in-flight work may be cancelled or allowed to finish opportunistically
- no partially complete semantic result is published

### 8.3 Executor death

If an executor dies mid-build:

- leased outputs may exist in CAS
- no committed result is published unless publication completed
- the computation is retried or reported failed
- orphaned leased objects are eventually reclaimed

### 8.4 Partial uploads

Partial uploads must not become published semantic objects.

Acceptable approach:

- upload content under a lease
- verify completeness and digest
- only then allow publication into a committed result

### 8.5 Daemon absence

The daemon/watcher may be absent and correctness must remain intact.

That means:

- file events are optional optimization hints
- one-shot CLI evaluation must still be able to compute correct results from
  content and store state alone

### 8.6 Crash recovery guarantees

The minimum correctness guarantee should be:

> Any published semantic value is durably reconstructible from the store.

The system is not required to preserve every provisional execution artifact
 after crashes. It is required not to publish references to data that is not
 durable.

## 9. Implementation Plan

### 9.1 Must-have for correctness

The following are architecture-critical and must be present even in an MVP:

- explicit input declaration for `exec`
- sandboxed execution
- content-addressed store
- computation identity and memo table
- publication boundary for committed results
- leases for in-flight outputs
- deterministic tree canonicalization
- value equality for at least the core semantic types
- content-based invalidation
- external demand roots

Without these, the architecture is no longer the one described here.

### 9.2 Can come later

These are important, but they are not required to preserve the architecture:

- remote execution across many machines
- advanced stream combinators
- rich observability API
- aggressive GC heuristics
- full closure-aware function identity
- sophisticated cancellation heuristics
- partial evaluation of more standard-library layers into native code/JIT

### 9.3 Proposed milestones

**Milestone 1: semantic core and local store**

- define core value model: Blob, Tree, Stream, TypedOutputs
- define computation identity and memo entry format
- implement local CAS
- implement publication vs lease distinction

**Milestone 2: local evaluator + local `exec`**

- interpreter/evaluator for pure computations
- local sandboxed `exec`
- declared structured inputs/outputs
- final committed typed results only

**Milestone 3: monotonic streaming primitive**

- implement `acquire(...) -> Stream<TreeEntry>`
- implement `collect_tree()`
- prove lease/publication/GC model on toolchain extraction

**Milestone 4: value-based incrementality**

- implement structural equality for parsed/typed values
- implement early cutoff
- demonstrate manifest reformatting that does not trigger downstream rebuilds

**Milestone 5: remoteization**

- executor protocol
- remote CAS/store access
- VFS projection for remote sandboxes

**Milestone 6: language ergonomics**

- parser/CST/LSP around the now-stable semantic core
- standard library in Vixen for Rust/C/OCI/toolchain workflows

### 9.4 Narrower version if the ambition is too high

This design can collapse under its own ambition if you try to build all of
 these at once:

- a new language
- a new incremental evaluator
- remote execution
- semantic streams
- rich observability

The narrower version that preserves the essence is:

- local orchestrator
- local store
- local sandboxed `exec`
- no remote execution yet
- memoization only for top-level named functions
- `acquire` as the only semantic stream primitive in v0
- no generic stream joins/folds/zips
- no user-visible event streams for `exec`

If even that feels too large, narrow further. Do not fake distributed semantic
 caching before computation identity is actually sound.

## 10. Open Questions / Risks

### 10.1 Stable computation identity is still the biggest risk

Hard question:

- how exactly are function identity and code changes represented?

Plausible options:

1. top-level named functions only in v0
2. structural identity of compiled HIR bodies
3. closure identity with captured environment hashing

Recommendation:

- start with 1 plus a path toward 2
- avoid 3 in the first architecture cut

### 10.2 Equality of semantic values can become a tarpit

Hard question:

- which types have structural equality
- which are opaque
- which require canonicalization

If equality is underspecified, early cutoff becomes handwaving.

Recommendation:

- restrict the early semantic boundary to types with canonical forms in v0

### 10.3 The design may be too eager to make everything memoizable

Risk:

- cache explosion
- expensive key computation
- too many useless retained intermediates

Recommendation:

- be selective about which computations become stable memo boundaries
- do not assume every intermediate should live forever just because it can

### 10.4 Semantic streams can sprawl

Risk:

- streams start infecting every API
- order semantics become muddy
- publication boundaries become unclear

Recommendation:

- keep semantic streams narrow in v0
- allow them only for clearly monotonic producers
- do not make generic `exec` streamy

### 10.5 Publication semantics for dynamic trees must be precise

The design assumes:

- output shape may be declared as "tree"
- actual contents discovered during execution

That is coherent, but the commit protocol must be exact. If publication of a
 dynamic tree is fuzzy, GC and cache correctness will be fuzzy too.

### 10.6 There is a real risk of accidentally rebuilding Bazel with more syntax

This happens if:

- pure computations stop mattering
- everything becomes a wrapper around `exec`
- value equality/early cutoff are not actually implemented

If that happens, the system is no longer the one described here. It becomes a
 task runner with better ergonomics and a heavier runtime.

## 11. Glossary

- **Blob**: immutable byte content stored by digest
- **Artifact**: typed reference to stored content plus semantic metadata
- **Tree**: canonical hierarchical collection of named artifacts/subtrees
- **TreeEntry**: one entry discovered within a tree-producing operation
- **Stream<T>**: progressive, reusable, unordered collection of typed elements
- **TypedOutputs**: final committed typed result record from `exec`
- **Computation identity**: key describing the semantics of one memoized
  computation
- **Lease**: temporary liveness for in-flight stored objects
- **Root**: durable reachability anchor for published results
- **Published**: part of a committed semantic result reachable from a root
- **Early cutoff**: skipping downstream recomputation when a rerun yields a
  semantically equal result

## 12. Architectural Invariants

- Demand is external to the language.
- Bindings denote typed values, not scheduled steps.
- Dependencies may be discovered dynamically during evaluation.
- Correctness must not depend on scheduler order.
- `exec` is semantically deterministic over declared inputs and environment.
- Generic `exec` does not expose provisional filesystem mutations as semantic
  values.
- Monotonic producers may yield semantic streams.
- File watching is a hint; content and semantic equality are authoritative.
- Published results must be durably reconstructible from the store.
- Leased in-flight outputs must not be confused with published semantic values.
- Early cutoff depends on stable equality, not on byte identity alone.
- Remote and local execution must share the same semantics.

## 13. Unresolved Questions Before Implementation Starts

- What is the exact representation of function identity and code/version
  invalidation?
- Which semantic types get canonical equality in v0?
- What is the exact memo-table schema for published values and deterministic
  failures?
- How is `download` represented: blob/file handle, bytes, or something richer?
- Which monotonic primitives besides `acquire` are in v0?
- What is the exact publication protocol for dynamic tree outputs from `exec`?
- How are late-bound inputs refreshed and pinned without introducing ambient
  nondeterminism?
- What is the precise boundary between runtime telemetry and any future
  observability API?

## 14. Proposed MVP Cut

The MVP that preserves the architecture without pretending to do everything is:

- local orchestrator only
- local CAS/store only
- local sandboxed `exec`
- explicit structured `exec` inputs/outputs
- `exec` returns only final committed typed outputs
- `acquire` as the only semantic stream primitive
- pathless file values and first-class trees
- leases + roots + publication distinction
- content-based invalidation
- memoization for top-level named functions and host primitives
- structural equality for a small core set of types
- no generic stream zip/fold/join/group-by
- no source-visible provisional `exec` event streams

If this MVP does not work, the larger system will not work. If it does work, it
 preserves the architectural essence and leaves room for remote execution,
 richer language features, and broader standard-library coverage later.
