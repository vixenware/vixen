+++
title = "Vixen language"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-05-26"
+++

The build language Vixen evaluates. Stays internal at Tier 2; becomes a public API contract at Tier 3.

## Domain logic lives in Vixen, not in the runtime

The Rust runtime is a generic engine. It knows how to evaluate Vixen, mount a sandbox+VFS, talk HTTP, hash bytes, run a PubGrub constraint solver, parse TOML/YAML/JSON into typed Vixen values. It knows nothing about cargo, Rust crates, Xcode, protobuf, or any specific language.

All of that lives in Vixen packages. `@vixen/rust` knows cargo: how a workspace is shaped, how features resolve, what `[patch]` does, what `[build-dependencies]` vs `[dependencies]` means semantically. `@vixen/c` knows clang and MSVC. `@vixen/protobuf` knows protoc. Adding a language is adding a package, not changing the runtime.

This is from day one. The cargo resolution model is Vixen code, not Rust code. The runtime supplies PubGrub as a generic SAT-shaped constraint solver; `@vixen/rust` supplies cargo's semantic model on top.

### Consequences

- **Caching of domain logic is free.** Evaluating "what's the dep graph of this workspace" is a Vixen expression. Same inputs → same dep graph → cache hit. No bespoke cargo-resolution-cache.
- **Versioning matters early.** Customers lock specific versions of `@vixen/rust`. Upgrading is a deliberate action with a downgrade path. The package mechanism is alive from Tier 2 onwards.
- **Performance lives in the runtime.** If `@vixen/rust`'s resolver is slow on huge workspaces, the answer is bytecode / JIT / better intrinsics — not "rewrite `@vixen/rust` in Rust." See the bytecode-and-JIT Task.
- **Runtime API surface stays small and stable.** What's exposed as intrinsics is the FFI; everything else is Vixen.

## Language stays internal until Tier 3

A separate strategic commitment. Tier 1 ships with no language exposure. Tier 2 ships with the language as substrate — customers run `vx cargo build` but never author `.vx`, never see the language. Tier 3 is the first tier where customers write Vixen.

Concretely:
- At Tier 1 / Tier 2, no `vx` CLI authoring surface, no FSKit-on-macOS instructions in customer-facing docs, no `.vx` authoring tutorials, no stability commitments. The language can iterate freely under real-world load.
- At Tier 3, customers author `.vx`. Stability commitments start: semantic versioning, deprecation cycles, migration tooling, LSP, IDE support.

Two years of Tier 2 customers using Vixen to build their cargo workspaces means the language and stdlib have seen real-world patterns before any external developer writes against them. The Tier 3 launch surface is informed by data, not speculation.

## FFI shape: body-absent declarations are intrinsics

A function declared in Vixen with **no body** is a runtime intrinsic. The runtime supplies the implementation. Looking at `crates/vx-stdlib/std/build.vx`:

```
fn exec[T](
  args: Array[ExecInput]
) with {
  env: Map[String, ExecInput] = %{},
  observer: Fn(Process, Tree) -> T,
} -> T;
```

The trailing semicolon, no body — that's an intrinsic. No special annotation, no `#native` effect, no `extern` keyword. The mechanism is invisible at the call site: callers don't know (or care) whether the callee is body-Vixen or body-Rust.

### Parser: Rust parses into Vixen types

A specific instance. When `@vixen/rust` needs to read a `Cargo.toml`, it declares a Vixen struct shaped like the cargo manifest (rename / alias / optional annotations on fields, like a serde struct definition), then calls the parsing intrinsic. The Rust runtime parses the TOML bytes *into* that Vixen type using the schema declared on the Vixen side. Schema lives in Vixen; parser lives in Rust.

### Current intrinsics

From `core.vx` and `build.vx`:
- Process execution under sandbox: `exec`, plus everything in the observer signature.
- Content-addressed fetch: `acquire(url) with {digest}`, `acquire(url) with {digest, extract}`.
- Host discovery: `discover[T](class, probe) -> T` — the probe takes a `Host` value provided by the runtime.
- Template rendering: `render[T](template) with {bindings} -> Blob`.
- Stabilisation across builds: `stabilize`.
- Operations on `String`, `Symbol`, `Tree`, `Stream`, `Map`, `Array`, `Path`, `Blob`, `Bytes`, plus the `Toml`/`Json`/`Yaml`/`Http` namespaces.

The list grows when the runtime needs a new primitive (a new sandbox capability, a new solver algorithm). It does *not* grow when domain logic changes — that's a Vixen-package concern.

The stdlib in the repo today still contains `read_file`/`read_dir`/`glob`/`exists`/`metadata` as free `#fs`-effect intrinsics plus a `build(entry)` intrinsic taking a closure. Both are legacy from before the design simplifications below; user-facing build code uses the `Tree`-as-workspace + `fn main`-direct-entry pattern instead, and host-filesystem access is reachable only through a `Host` capability obtained from a discovery probe.

## Values are hashable, comparable, serialisable

A core language-level property. Every value the runtime produces — scalar, struct, enum variant, function, closure, tree, blob, stream — is required to be:

- **Hashable.** A canonical content hash exists for any value. Two values with the same hash are interchangeable; equal values have the same hash. The hash mixes into cache keys.
- **Comparable.** Equality is decidable. Same canonical form ⇒ equal; distinct canonical forms ⇒ not equal. No value-identity-via-pointer.
- **Serialisable.** Bytes exist. A value can be written, transmitted, stored content-addressed, fetched back, reconstituted into the same value.

Implemented for most value shapes today. Closures and streams are still maturing — a stated design goal that every value eventually be fully serialisable; some shapes still need work.

### Why this property

The cache, the executor fleet, the closure-shipping story, the demand-warmed flow, the build-graph-is-provenance property all reduce to "I can hash this, I can ship it, I can verify it came back equal."

- Caches store and retrieve any `T`; non-serialisable `T` couldn't be cached.
- Closure shipping requires serialising the closure.
- Demand signals are serialised onto the queue.
- Equality is provenance: "this output was produced from these inputs" reduces to "hash(inputs) → handle(output)."

### Consequences

- No identity comparisons. Values compare by content, not by allocation site.
- No mutable references at the language level. A value can't change content without becoming a different value.
- No reflection on internal layout. The language exposes the canonical form; the runtime picks the in-memory representation.
- Wire format is implicit. The language doesn't expose a "write to bytes" primitive; values flow through cache and wire transparently.

## Closures ship as canonical IR

A direct consequence of "values are hashable, comparable, serialisable" applied to closures. When an `exec` runs on a remote executor, its `observer: Fn(Process, Tree) -> T` ships to that executor as a value like any other.

The wire form is **canonical Vixen IR** — sorted record fields, normalised binder names, deterministic ordering for sets/maps, no source-text whitespace or comments. Two closures that mean the same thing hash to the same bytes regardless of where they were typed. Captures are recursively values, recursively canonical; the closure's content hash is the hash of the IR mixed with each capture's hash.

The receiving executor needs a compatible runtime to evaluate the IR. That compatibility is an executor ability — `struct VixenRuntime { version: SemVer, intrinsic_set: Hash }` — pinned by the build's predicate. Mismatch is a routing failure caught before dispatch, not a mid-build error. See [Executor abilities](/executor-abilities/).

Failure modes are idempotent. A closure that trips a runtime invariant returns a structured `Diagnostic` and poisons the exec; a lost-mid-eval failure is retryable on another eligible executor; a missing intrinsic marks the executor stale on that intrinsic. Retries are safe because nothing the closure does on the executor is observable elsewhere until the exec result is.

## Syntax: kwargs use `with`, not `where`

Function signatures separate positional arguments from named arguments with defaults via a `with { … }` block:

```
fn exec[T](
  args: Array[ExecInput]
) with {
  env: Map[String, ExecInput] = %{},
  observer: Fn(Process, Tree) #sandbox -> T,
} -> T;
```

Earlier the keyword was `where`. That collided with Rust's generic-bounds `where` and created persistent reader confusion. `with` reads sentence-wise: "exec args, with env and observer." No conflict with Rust precedent.

Mechanical migration: every `where { … }` on function signatures becomes `with { … }`. `where` becomes available as an identifier; `with` becomes reserved.

## No effect annotations — capabilities are object types

The language has **no effect-annotation system** (`#fs`, `#discover`, `#host`, `#sandbox` and so on are not in the user-facing surface). Capabilities are conveyed by parameter types alone — object-capability style. Whoever has a reference to an object of a capability-carrying type has the runtime's permission to do what that type allows. Whoever doesn't, can't.

### Non-constructible types

Capability-bearing types are **non-constructible by user code.** Only the runtime can hand you one:

- **`Host`** — runtime gives the user code a `Host` value as a parameter to a probe (the closure passed into the `discover` intrinsic). User code can't manufacture a `Host`; there's no public constructor. A probe holds one for its duration; it can call methods on it (`host.os`, `host.which`, `host.read`, `host.exec`); when the probe returns, the runtime drops the `Host` reference.
- **`Process` and the sandbox-output `Tree`** — runtime gives these as parameters to the observer of an `exec` call. Same non-constructibility property. The observer can read from the `Process` (stdout / stderr / exit code) and the `Tree` (write-set); when it returns, the references drop.

The type's existence in a function's scope conveys the authority — nothing else needed.

### Operations on a `Host` are recorded as obligations

`Host` is a live mutable host. It can't be content-addressed. So whenever user code calls a method on a `Host` value, the runtime **records the call + its observed result** as part of the value the probe will return. The recorded tuple is the **obligation**.

A probe returning, for example, a `MacToolchain` value: the returned struct's identity (for caching purposes) is derived from the recorded operations + their results. Downstream nodes consuming the `MacToolchain` (an `exec` that mounts the toolchain at compile time) incorporate that identity into their own cache keys, transitively.

**Verification races with the work** that consumes the capability. The runtime re-runs the recorded operations against the live host; if a recorded op now produces a different result, the obligation is broken. Poison-or-take-notes per the rules in [Executor abilities](/executor-abilities/).

### Operations on `Process` and `Tree` don't need recording

`Process` and `Tree` from an `exec` are already content-addressed by the sandbox's read-set discipline. Their methods are pure functions on declared inputs — no hidden world state. No obligation needed.

### What the probe context can do is narrower than what an observer can

The exec observer gets a rich `Process` (stdout / stderr / exit code) plus a write-set `Tree`. A probe doesn't — `host.exec(args)` returns a small typed result (the output bytes; exit code) rather than a full `Process` value. The probe context surface stays narrow to keep the obligation tuple small and the recorded operations tractable.

This is type-level separation: probe-context author never holds a `Process` value; observer never holds a `Host` value. The runtime never crosses them.

### Cache-friendliness comes from non-impurity, not from positive marking

A function that only takes content-addressed parameters and produces a value from them is *trivially cacheable*. No annotation needed — its identity is its source plus its parameters' identities.

A function consuming a `Host` is **not** trivially cacheable in the same way; its result depends on operations recorded against the host. The runtime handles that as a special case driven by the `Host` type, not by a `#host` annotation propagating through call chains.

The result is that build code authors don't think about effects at all. They consume the typed values they're given, return typed values, and the runtime takes care of the rest.

## Package entry is `fn main`, called directly

Each Vixen package that ships a `vx`-level subcommand exports a function named `main`. `vx`-the-CLI invokes `main` directly; there's no wrapper intrinsic.

```
fn main(workspace: Tree) with {
  args: Array[String],
  env: Map[String, String],
} -> Tree
```

The workspace is positional; `args` and `env` are keyword arguments (the `with` block). `main` always returns a `Tree`.

### The returned Tree overlays onto the workspace

The Tree `main` returns has **workspace-relative paths**, and `vx` merges it onto the customer's workspace — overlay semantics, not replace. `@vixen/cargo::main` returns `Tree { "./target/release/foo": <bin>, "./target/release/build/...": ..., "./Cargo.lock": ... }`; `vx` lands `./target/...` and updates `./Cargo.lock` because that's where the Tree said they go. The `./target/` convention lives in `@vixen/cargo`'s implementation, not in `vx` or the language.

A minimal subcommand can be as small as "return `Tree { "./out/app": <bin> }`". Make, Bazel, and cargo all spent decades layering convention on top of that statement; the language reduces it to *the Tree is what the workspace should look like after the build*.

Mechanism of the overlay (copy vs VFS-mount vs hard-link) is open — see [Design build-output materialisation](/materialise-build-output/).

The current stdlib has a `build(entry: Fn(...) -> T) -> T;` intrinsic that takes a closure — that's legacy. The new convention removes that wrapper.

## Demand-driven evaluation

`let x = exec [...]` does not run anything. It produces a graph node — a thunk identified by its inputs — and binds `x` to it. Same for `acquire`, `render`, `pubgrub_solve`, and every other intrinsic that does substantial work. The body of `main` is a graph-construction phase: the evaluator threads values through the user's code, but no work happens.

Forcing — actually computing — is triggered by **demand from the host boundary**. The `vx` CLI forces `main`'s return value. Forcing a `Tree` lazily forces its entries; forcing a `Blob` entry forces the `exec` node that produced it; transitively. Subgraphs no demand reaches never run. `vx cargo build -p foo` evaluates the dep graph reachable from `foo` and ignores the rest of the workspace.

Within a single evaluation, the evaluator memoises every demanded thunk by its identity. Two demands for the same node share one in-flight evaluation. Across evaluations, the persistent cache plays that role — for the subset of intrinsics whose results are worth storing.

### Persistent cache cutoff is per-intrinsic

Not every value's identity is worth materialising to disk. Pure arithmetic, value construction, string operations, equality — cheaper to recompute than to round-trip through a cache. The persistent cache writes only the results of intrinsics that did external or substantial work: `exec`, `acquire`, `Http`, `Toml/Json/Yaml::parse`, `pubgrub_solve`, `render`, `stabilize`, and the recorded-obligation tuples from `discover`. Trivial intrinsics (`String::concat`, equality, hash, arithmetic on scalars, Tree projection) and all pure Vixen function calls stay in the per-evaluation memo table.

The cutoff is a property of each intrinsic, decided in the runtime, not a user-visible knob. *Cacheable by the type system* (every value can in principle be cached) is not the same as *worth caching* (the lookup cost beats the recompute cost). See [Cache and trust → Which intrinsics get persisted](/cache-and-trust/#which-intrinsics-get-persisted) for the categories, the dispatch mechanism, and the treatment of pure user functions.

## Cargo-resolution-as-Vixen, lockfiles-as-outputs

A consequence worth calling out for the cargo-replacement use case.

`@vixen/rust`'s resolver is Vixen code. It reads `Cargo.toml`, talks to the crates.io registry (via the `Http` intrinsic), runs PubGrub (intrinsic) over the constraints, emits a `Cargo.lock` byte-identical to upstream cargo's.

The resolution operation is a build-graph node: inputs are `Cargo.toml` + the registry view at this moment + the resolver code's version; output is `Cargo.lock`. The fact that the output lands at the conventional on-disk path is a file-system convention, not a semantic mutation. The cache stores `(inputs → Cargo.lock)`; same inputs → same lockfile → instant. New version published → different registry view → cache miss → re-resolve.

`cargo update` is just "force re-resolution against current registry state, ignore prior lockfile."
