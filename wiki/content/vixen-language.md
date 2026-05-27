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
  observer: Fn(Process, Tree) #sandbox -> T,
} -> T;
```

The trailing semicolon, no body — that's an intrinsic. No special annotation, no `#native` effect, no `extern` keyword. Inputs, outputs, capability annotations on the signature behave like any other function. The mechanism is invisible at the call site: callers don't know (or care) whether the callee is body-Vixen or body-Rust.

### Parser: Rust parses into Vixen types

A specific instance. When `@vixen/rust` needs to read a `Cargo.toml`, it declares a Vixen struct shaped like the cargo manifest (rename / alias / optional annotations on fields, like a serde struct definition), then calls the parsing intrinsic. The Rust runtime parses the TOML bytes *into* that Vixen type using the schema declared on the Vixen side. Schema lives in Vixen; parser lives in Rust.

### Current intrinsics

From `core.vx` and `build.vx`:
- Process execution under sandbox: `exec`, plus everything in the observer signature.
- Content-addressed fetch: `acquire(url) with {digest}`, `acquire(url) with {digest, extract}`.
- Local-host filesystem (under `#fs`): `read_file`, `read_dir`, `glob`, `exists`, `metadata`.
- Build evaluation entry: `build(entry) -> T`.
- Host discovery: `discover[T](class, probe) #discover -> T`.
- Template rendering: `render[T](template) with {bindings} -> Blob`.
- Stabilisation across builds: `stabilize`.
- Operations on `String`, `Symbol`, `Tree`, `Stream`, `Map`, `Array`, `Path`, `Blob`, `Bytes`, plus the `Toml`/`Json`/`Yaml`/`Http` namespaces.

The list grows when the runtime needs a new primitive (a new sandbox capability, a new solver algorithm). It does *not* grow when domain logic changes — that's a Vixen-package concern.

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

## Effects: `#fs`, `#discover`, `#host`, `#sandbox`

Capability annotations on function signatures, declared at the type level. They denote *where* the function expects to be evaluated:

- `#fs` — local-host filesystem access. `read_file`, `glob`, etc.
- `#discover` — using a representative executor to learn host facts. `discover(...)` itself, plus the probe it accepts.
- `#host` — host-inspection authority inside an executor's probe.
- `#sandbox` — running inside an executor's sandbox. Observers in `exec` are `#sandbox`.

Effects propagate through call sites. The full semantics — declared vs inferred, static vs runtime checked — are documented in the language spec elsewhere in the tree. The salient property for build-system purposes: a function's effect signature is part of its type, so the cache key for any call mixing effect handling captures which authority context the call evaluated under.

## Cargo-resolution-as-Vixen, lockfiles-as-outputs

A consequence worth calling out for the cargo-replacement use case.

`@vixen/rust`'s resolver is Vixen code. It reads `Cargo.toml`, talks to the crates.io registry (via the `Http` intrinsic), runs PubGrub (intrinsic) over the constraints, emits a `Cargo.lock` byte-identical to upstream cargo's.

The resolution operation is a build-graph node: inputs are `Cargo.toml` + the registry view at this moment + the resolver code's version; output is `Cargo.lock`. The fact that the output lands at the conventional on-disk path is a file-system convention, not a semantic mutation. The cache stores `(inputs → Cargo.lock)`; same inputs → same lockfile → instant. New version published → different registry view → cache miss → re-resolve.

`cargo update` is just "force re-resolution against current registry state, ignore prior lockfile."
