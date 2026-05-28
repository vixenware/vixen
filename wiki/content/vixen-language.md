+++
title = "Vixen language"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-05-26"
+++

The build language Vixen evaluates. Stays internal at Tier 2; becomes a public API contract at Tier 3.

## Domain logic lives in Vixen, not in the runtime

> **This is settled and non-negotiable.** "Just shell out to cargo / reuse an existing tool" is not an open question — it throws away the entire value proposition. See [FAQ: settled decisions](/faq/) before raising it.

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
- Ability request: `Type where (boolean expression)` — predicate-identified value; planner resolves at exec sites. See [Executor abilities](/executor-abilities/#builds-request-abilities-at-runtime). Host probing itself is not a user-Vixen-callable intrinsic; it happens inside the daemon, reading from capability probe packages (see [Executor abilities → How abilities are advertised](/executor-abilities/#how-abilities-are-advertised)).
- Template rendering: `render[T](template) with {bindings} -> Blob`.
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

## Function application is the single primitive

There is one syntactic form for "do something with a value": **`F payload`** — function `F` applied to payload, juxtaposition with self-delimiting payloads (or `F(payload)` with parens). Constructors, projections, lookups, calls — all the same shape.

```vixen
import "vixen:rust"                          // import applied to a string
Tree { "foo" => 42, "bar" => 99 }            // Tree (constructor) applied to a map-shaped payload
exec [rustc, src, "-o", "out"]               // exec applied to an array-shaped payload
workspace "src/main.rs"                      // workspace (a Tree, callable as String -> Value) applied to a string
(exec […]) "app"                             // the result of exec, itself a Tree, applied to a string
```

The Tree type is itself callable — `tree "some/path"` gives the value at that path. There is no separate "projection operator." Maps work the same way; arrays work the same way. Records use `.field` because field-projection is statically typed and known at compile time; that's the only case using different syntax.

Self-delimiting payloads (no parens needed for juxtaposition): `(...)`, `[...]`, `{...}`, `%{...}`, `"..."`, `` `...` ``.

The chain `exec [...] "app"` parses left-associatively as `(exec [...]) "app"` — first apply `exec` to the array (producing the write-set tree), then apply that tree to the string-shaped path `app`. No operator precedence to think about.

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

Mechanical migration: every `where { … }` on function signatures becomes `with { … }`. `with` becomes reserved. `where` is reserved for the [constraint-expression](#constraint-expressions) construct below — different syntactic role, no overlap with the old kwargs use.

## Imports

Modules are records. The `import` expression returns a module value, bound with `let`:

```vixen
let config = import "vixen:config"
let { stable } = import "vixen:rust"
```

`import` is an ordinary function; the string identifying the target is its single payload, applied by juxtaposition (no parens needed — strings are self-delimiting). Destructuring lets pull selected exports.

The string identifies the target:

- **`vixen:<name>`** — stdlib or first-party package (`@vixen/...` scope).
- **Bare name** — workspace-local sibling `.vx` file, or a package declared as a dependency in the manifest.

Same shape across all three sources (stdlib, cross-package, intra-package). Dotted access on the returned record reaches the module's exports: `config.linux_executor`, `c.discover_toolchain`.

Stdlib intrinsics (`exec`, `acquire`, `Toml::parse`, …) are in scope via an implicit prelude — no explicit `import("vixen:core")` needed. Users writing build code see the intrinsic surface as ambient identifiers.

## Brackets: `( ... )` groups, `{ ... }` is structural, `[ ... ]` is positional

The three bracket pairs carry distinct roles, and the distinction is load-bearing for readability.

### `( ... )` — expression grouping (including function bodies)

`( ... )` always means "an expression, possibly a sequence of statements with the last being the value." This includes:

- **Expression grouping for precedence.** `(a + b) * c`.
- **Function call payload.** `f(x)` is `f` applied to `x` (one positional payload).
- **Function declaration parameters.** `fn foo(x: Int, y: Int)`.
- **Tuples.** `(1, "a", true)` when there are multiple positional elements.
- **Function bodies.** `fn name(params) -> T (body)`. Same `( ... )` as everywhere else — a function body really is "a grouped expression whose value is the final inner expression."
- **Closures returning blocks.** `(x) => (let y = x + 1; y)`.

Multi-statement form uses `;` as the separator; the last expression (without trailing `;`) is the value. Example with intermediate steps:

```vixen
fn build_app(workspace: Tree) -> Tree (
    let rust = RustToolchain where (version >= 1.91);
    let bin = exec [rust.rustc, workspace "src/main.rs", "-o", "out/app"] "app";
    Tree { "target/release/app" => bin }
)
```

The "function body uses parens" choice is the most visually unusual decision the language makes. It buys uniformity: `( ... )` is one thing everywhere; you never have to scan inside to know what kind of bracket you're reading.

### `{ ... }` — structural values with named entries

`{ ... }` always means "a value whose structure is a set of named entries." The entries' separator inside is always `,` (or newline; the two are interchangeable). Never `;` — semicolons live only in `( ... )`.

Modes, distinguished by what's *inside* the braces:

| Mode | Inner shape | Example |
|---|---|---|
| Record literal | `field: value` | `Point { x: 1, y: 2 }` |
| Map / tree literal | `key => value` | `Tree { "k" => v }` |
| Spread | `..other` | `with { ..VixenMain }` |

Kwargs application reuses the record form: `f payload with { x: 1, y: 2 }` is `f payload` followed by `with` applied to a record. Struct update is the same — `value with { field: new_value }` applies `with` to a record of updates.

The previously-considered "constraint block" mode (`where { … }` with comma-separated clauses) is gone — constraints are expressions in `( ... )`, see [Constraint expressions](#constraint-expressions).

### `[ ... ]` — positional sequences and type parameters

`[ ... ]` is positional:

- Array literals: `[1, 2, 3]`.
- Type parameters: `Array[String]`, `Fn[T]`.
- exec args: `exec [rustc, src, "-o", "out"]` is `exec` applied to an array payload.

### Why this matters

`{ ... }` does six different things in many languages because everyone reaches for braces by default. Vixen splits the workload across three bracket pairs along a meaningful axis:

- Sequencing of operations → `( ... )` (grouped expressions, blocks).
- Naming of structural data → `{ ... }` (records, maps, kwargs).
- Positional listing → `[ ... ]` (arrays, type params).

When you see one, you know which family the contents belong to without having to scan inside. The cost is one weirdness budget item (function-body parens); the win is everywhere.

## Constraint expressions

A first-class language construct for expressing *structural* requirements over typed values — the build-language equivalent of SQL's `WHERE` clause, scoped to a single type. Used pervasively: ability requests against the executor fleet, cargo-style dependency declarations, version-range constraints for any package manager, target predicates, anywhere a build queries structured values by structural predicates.

### Shape

`T where (expression)` produces a typed `Requirement[T]` value. The expression is a closure body returning a `Constraint[T]` (or a `Bool` derived from one). The candidate is bound to `$` (the language's standard implicit-closure parameter); inside the `where (...)` parens, a free identifier `field` desugars to `$.field` (see [Implicit-`$.` sugar](#implicit-sugar-inside-where) below).

```vixen
let rust = RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")

let serde = Crate where (name == "serde" && version ^= 1.0 && features contains "derive")
```

Multi-line via the regular expression-grouping `( ... )`:

```vixen
let rust = RustToolchain where (
    version >= 1.91
    && targets contains "aarch64-apple-darwin"
)
```

Disjunction, negation, and any other boolean composition come for free because the body is an ordinary expression:

```vixen
let rust  = RustToolchain where (version ^= 1.91 || version ^= 1.90)
let safe  = Crate where (!(features contains "unstable") && version >= 1.0)
```

### Constraint operators are structured expressions

Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`), collection operators (`contains`, `in`, `matches`), and type-extensible operators (SemVer's `^=` / `~=`, etc.) **do not return `Bool` directly**. They return a structural value (call it `Constraint[T]`) that **coerces to `Bool`** at use sites where a `Bool` is what's wanted. Same pattern as LINQ Expression Trees in C# — a lambda compiles either as a delegate or as an introspectable AST depending on context — and the EDSL pattern in Rust's Diesel, Python's SQLAlchemy core, etc.

Three contexts, three behaviours, same expression:

```vixen
// Both operands concrete — Constraint coerces to Bool immediately
if x.version >= 1.91 ( ... )

// Inside a where (...) — the closure's IR carries the structural form
RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")

// As a standalone value — the structure is preserved
let bound = SemVer.^=(1.91)        // Constraint[SemVer]
let combined = bound && other       // structurally composed
```

`&&`, `||`, `!` are overloaded analogously: on two `Bool`s they're plain boolean operators; on `Constraint[T]`s they're structural intersection / union / negation; on mixed (one concrete, one structural) they coerce the concrete side and compose structurally.

### Why structured rather than closure-only

The orchestrator (and any consumer of `Requirement[T]`) needs to index, compose, hash, and produce error messages over constraints. An opaque closure is evaluatable against candidates but not introspectable. Structured expressions give us:

- Indexing — the registry of advertised abilities can answer `version >= X` queries by lookup rather than scan.
- Canonical hashing — two equivalent requirements share a cache key naturally.
- Specific error messages — "you required `version >= 1.91`; available: 1.89, 1.90."
- Composition — `req_a && req_b` produces a combined structural requirement, not a closure-of-closures.

Vixen's IR is already canonical and inspectable, so any closure handed to a `where` is analyzed for the structural form. Closures the analyzer can pattern-match (any expression built from the operators above) yield a `Constraint[T]`; closures with patterns the analyzer can't recognize fall back to opaque-per-candidate evaluation — still correct, just not indexable.

### Implicit-`$.` sugar inside `where (...)`

Inside the `( ... )` payload of a `where`, free identifiers desugar to `$.identifier`:

```vixen
// these two are equivalent:
RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")
RustToolchain where ($.version >= 1.91 && $.targets contains "aarch64-apple-darwin")
```

The sugar is narrow grammatical scope. Outside the `where (...)` parens, no such resolution rule applies — `field` is just `field`.

**Explicit local bindings still win.** If `version` is bound in the enclosing scope, the bare identifier refers to that binding, not the candidate's field. To force the field-resolution, use `$.version`. A free identifier matching neither is a compile-time error.

Similar in spirit to LINQ's query syntax (`where x.Age > 18` binds `x` from the enclosing `from`) and Rust's struct-destructuring patterns. Limited mode by position, predictable resolution rules.

### Type-extensible operator set

Built-in operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `contains`, `in`, `matches`. Types declare their own via namespace functions:

```vixen
namespace SemVer (
    fn (^=)(other: SemVer) -> Constraint[SemVer] (CaretMatch other);
    fn (~=)(other: SemVer) -> Constraint[SemVer] (TildeMatch other);
)
```

(Function bodies are `( ... )` per the new convention; see [Function application is the single primitive](#function-application-is-the-single-primitive).)

Then `version ^= 1.0` resolves to `SemVer.(^=)(1.0)`, returning a structural `Constraint[SemVer]`. Cargo's caret / tilde / inequality matchers, cfg-expression matching, JSON-path matching — all extend the constraint vocabulary without changing the language.

### Numeric literal rule (build-language flavour)

Vixen is a build language; semver is the common case for dotted-numeric values, floats are the unusual case.

- `0.13`, `1.0`, `1.0.0`, `1.0.0-rc.1` — `SemVer` literals. Two-or-three segment, optional pre-release / build metadata.
- `1.34f` — `F64` literal. Floats require the `f` suffix.
- `42`, `1_000_000` — `U64` literals as before.

This kills the tension between "looks like 0.13 = serde version" and "looks like 0.13 = float" — in build code the answer is always SemVer unless the `f` suffix says otherwise. Matches what builders actually write in `Cargo.toml`.

## No effect annotations — capabilities are object types

The language has **no effect-annotation system** (`#fs`, `#discover`, `#host`, `#sandbox` and so on are not in the user-facing surface). Capabilities are conveyed by parameter types alone — object-capability style. Whoever has a reference to an object of a capability-carrying type has the runtime's permission to do what that type allows. Whoever doesn't, can't.

### Non-constructible types

Capability-bearing types are **non-constructible by user code.** Only the runtime can hand you one:

- **`Host`** — runtime gives a `Host` value to a capability probe (the `Type.probe(host)` function declared in a capability probe package; see [Executor abilities](/executor-abilities/#how-abilities-are-advertised)). User code can't manufacture a `Host`; there's no public constructor. The probe holds one for its duration; it can call methods on it (`host.os`, `host.which`, `host.read`, `host.exec`); when the probe returns, the runtime drops the `Host` reference. Probe code runs inside the daemon process, not in the user's build code.
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

```vixen
fn main(workspace: Tree) with { ..VixenMain } -> Tree (
    Tree { "target/release" => exec [stable.rustc, workspace "src/main.rs", "-o", "out/app"] }
)
```

The workspace is positional; the kwarg block uses the spread form `with { ..VixenMain }` to pull in the standard CLI kwargs (`args: Array[String]`, `env: Map[String, String]`) without spelling them out every time. `main` always returns a `Tree`.

The body is wrapped in `( ... )` per the [bracket conventions](#brackets-----groups-----is-structural-----is-positional). A single-expression body is one expression inside the parens; intermediate steps use `;` between statements with the final expression as the value.

**Path-prefix reduction.** Notice the Tree literal uses `"target/release" => exec [...]` rather than `"target/release/app" => exec [...] "app"`. Placing an exec result (a Tree) at a directory key grafts the whole subtree there — the binary that the exec wrote to `out/app` lands at `target/release/app` because the exec's write-set tree gets mounted at `target/release`. Common-case shorthand for "this directory is whatever the build wrote."

Paths in the example are ordinary strings. There is no separate path-literal syntax; the path-flavor comes from the value's provenance through Tree projection, not from how the literal was written. See [Paths are strings + provenance](#paths-are-strings--provenance) below.

### The returned Tree overlays onto the workspace

The Tree `main` returns has **workspace-relative paths**, and `vx` merges it onto the customer's workspace — overlay semantics, not replace. `@vixen/cargo`'s `main` returns a Tree like `Tree { "target/release/foo" => <bin>, "target/release/build/..." => ..., "Cargo.lock" => ... }`; `vx` lands `target/...` and updates `Cargo.lock` because that's where the Tree said they go. The `target/` convention lives in `@vixen/cargo`'s implementation, not in `vx` or the language.

A minimal subcommand can be as small as "return `Tree { "out/app" => <bin> }`". Make, Bazel, and cargo all spent decades layering convention on top of that statement; the language reduces it to *the Tree is what the workspace should look like after the build*.

Mechanism of the overlay (copy vs VFS-mount vs hard-link) is open — see [Design build-output materialisation](/materialise-build-output/).

The current stdlib has a `build(entry: Fn(...) -> T) -> T;` intrinsic that takes a closure — that's legacy. The new convention removes that wrapper.

## Demand-driven evaluation

`let x = exec [...]` does not run anything. It produces a graph node — a thunk identified by its inputs — and binds `x` to it. Same for `acquire`, `render`, `pubgrub_solve`, and every other intrinsic that does substantial work. The body of `main` is a graph-construction phase: the evaluator threads values through the user's code, but no work happens.

Forcing — actually computing — is triggered by **demand from the host boundary**. The `vx` CLI forces `main`'s return value. Forcing a `Tree` lazily forces its entries; forcing a `Blob` entry forces the `exec` node that produced it; transitively. Subgraphs no demand reaches never run. `vx cargo build -p foo` evaluates the dep graph reachable from `foo` and ignores the rest of the workspace.

Within a single evaluation, the evaluator memoises every demanded thunk by its identity. Two demands for the same node share one in-flight evaluation. Across evaluations, the persistent cache plays that role — for the subset of intrinsics whose results are worth storing.

### Persistent cache cutoff is per-intrinsic

Not every value's identity is worth materialising to disk. Pure arithmetic, value construction, string operations, equality — cheaper to recompute than to round-trip through a cache. The persistent cache writes only the results of intrinsics that did external or substantial work: `exec`, `acquire`, `Http`, `Toml/Json/Yaml::parse`, `pubgrub_solve`, `render`, and the recorded-obligation tuples from `discover`. Trivial intrinsics (`String::concat`, equality, hash, arithmetic on scalars, Tree projection) and all pure Vixen function calls stay in the per-evaluation memo table.

The cutoff is a property of each intrinsic, decided in the runtime, not a user-visible knob. *Cacheable by the type system* (every value can in principle be cached) is not the same as *worth caching* (the lookup cost beats the recompute cost). See [Cache and trust → Which intrinsics get persisted](/cache-and-trust/#which-intrinsics-get-persisted) for the categories, the dispatch mechanism, and the treatment of pure user functions.

## Errors are Diagnostics that poison the build

There is no `Result[T, E]` propagation surface for user code, no exception machinery. When an intrinsic fails — `Toml::parse` on malformed input, `acquire` on a digest mismatch, `Http` on a 404, `exec` on a sandbox violation — it emits a structured `Diagnostic` and **poisons** the demand chain rooted at the failing call. The build collects all Diagnostics that fired during evaluation and reports them at the end.

Process exit codes are a separate concern. A non-zero exit from a process the observer is reading is *not* an automatic poison — the observer receives the `Process` value (with its exit code) and decides what to do. "Compilation failed with this error message" is a value flowing through user code; "the runtime couldn't parse your TOML" is poison.

The explicit poison primitive is `fail "message"` — emits a Diagnostic and stops the current branch.

### No `?` recovery operator

Considered and rejected. A postfix `?` that catches poison and yields `Option` was on the table, modelled after Swift's `try?`. Walking through Rust / C / C++ / container-image building scenarios, the case for it didn't hold up:

- **Manifest parse failures, resolution conflicts, sandbox violations** — these *should* be fatal. Catching them yields a `None` you can't usefully act on.
- **Process exit codes from compilers and build scripts** — observable via the `Process` value in the observer; not a poison in the first place.
- **Tree lookups, feature-detection probes, optional config files** — design these to return `Option[T]` directly. Composition with `match` / `or_else` / `unwrap_or` is clear and explicit.
- **HTTP fetches with mirrors / retries** — runtime fetch policy concern, not user-code.

The pattern: recoverable operations in build code are naturally `Option`-returning by API design. The genuinely fatal cases have nothing useful to recover to. The middle space — "an operation that morally should succeed but might fail and you want to fall back" — doesn't exist in build code.

Without `?`, the footgun (silent Diagnostic swallowing) goes away. Recoverable failures are visible in type signatures; fatal failures end the build with a structured report. If a real use case shows up later, easier to add `?` than to remove it.

## Cargo-resolution-as-Vixen, lockfiles-as-outputs

A consequence worth calling out for the cargo-replacement use case.

`@vixen/rust`'s resolver is Vixen code. It reads `Cargo.toml`, talks to the crates.io registry (via the `Http` intrinsic), runs PubGrub (intrinsic) over the constraints, emits a `Cargo.lock` byte-identical to upstream cargo's.

The resolution operation is a build-graph node: inputs are `Cargo.toml` + the registry view at this moment + the resolver code's version; output is `Cargo.lock`. The fact that the output lands at the conventional on-disk path is a file-system convention, not a semantic mutation. The cache stores `(inputs → Cargo.lock)`; same inputs → same lockfile → instant. New version published → different registry view → cache miss → re-resolve.

`cargo update` is just "force re-resolution against current registry state, ignore prior lockfile."

## Paths are strings + provenance

There is no distinct path-literal syntax. Path-shaped values are ordinary strings, distinguished from "plain strings" by **value provenance** — the runtime tracks which strings originated as Tree projections (or were derived from them) and uses that provenance for cache keying and implicit mounting into sandboxes.

The path-vs-string distinction the user briefly sees in the literal form is fake: in `exec [stable.rustc, workspace "src/main.rs", "-o", "out/app"]`, the four literals are all strings. What differs operationally is whether each value has tree-provenance:

- `stable.rustc` — value from a `RustToolchain` projection. Has provenance. Sandbox sees the provenance and implicitly mounts the toolchain.
- `workspace "src/main.rs"` — Tree projection. The string `"src/main.rs"` is the projection key; the *result* of the projection has provenance. Mounted implicitly.
- `"-o"` — plain string. No provenance, no mount, just argv.
- `"out/app"` — plain string. The sandbox's writable-root rules govern the output location; the literal carries no provenance and needs none.

Quote style is never the discriminator. Provenance is.

### Why no distinct quote form

A distinct literal (e.g. `'src/main.rs'` for paths vs `"-o"` for strings) was considered and rejected. In realistic `exec` calls, most literal values are subprocess argv — `-o`, `--target`, `--edition=2021`, target triples, output paths. None of them have a meaningful semantic claim to being "a path" vs "a string"; they're tokens the subprocess interprets however it wants. Forcing the author to pick quote style would manufacture a question they can't meaningfully answer for most tokens, and the choice would either be arbitrary cognitive cost or a subtle footgun.

The DX win — "this token visibly looks like a path" — exists but is small, and it cost a syntactic distinction the language doesn't need.

### Where path-flavor surfaces in types

When something genuinely wants the path API (splitting components, joining, manipulating relative-vs-absolute, etc.), the methods live on the value type — `Path.split`, `Path.join`, and similar — not on the literal form. A string with tree-provenance can be inspected as a path through those methods because the runtime knows its shape.

### Coercion / loss of provenance

When a path-shaped value with provenance is concatenated or transformed in a way the runtime can no longer track (e.g. extracted into a hand-built `${segment}-suffix` template), provenance is lost at the transformation point. The resulting value is a plain string. If the user wanted to keep provenance through that operation, they should use a `Path` API that preserves provenance through joins. Detailed coercion rules to be specified at implementation time.
