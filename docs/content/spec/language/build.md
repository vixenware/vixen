+++
title = "Build and Evaluation"
insert_anchor_links = "heading"
+++

# Vixen Build and Evaluation Specification

## Store types

> r[store.blob]
>
> `Blob` is a single file in the content-addressable store (keyed by
> Blake3 hashes). `Blob(s)` creates a blob from a text string. Blobs
> with binary content come from `exec`.

> r[store.blob.utf8]
>
> `blob.utf8!` returns the blob's content interpreted as a UTF-8 string.
> It poisons if the content is not valid UTF-8. `blob.utf8_lossy!`
> always succeeds and replaces invalid sequences.

> r[store.tree]
>
> `Tree` is a directory with its full structure and filenames preserved.
> There is no separate "local tree" vs "store tree" type — the runtime
> handles materialization transparently.
>
> `Tree map` creates a tree from a map of paths to entries.
> `Tree.from_path path` creates a lazy tree handle rooted at a local
> filesystem path.
> `Tree {}` is the empty tree.
> `tree.subtree path` selects a subtree. `tree.glob pattern` filters by
> glob. `tree.merge other` merges two trees (later entries win). These
> operations are lazy.

> r[store.tree.file]
>
> `tree.file path` returns the `Blob` at the given relative path
> within the tree. Poisons if the path does not exist or points to a
> directory. The fallible version `(tree.file path)?` returns
> `Option[Blob]`.

> r[store.path]
>
> `Path` is its own type at the language level, implemented as a
> normalized slash-delimited path string rather than a plain `String`.
> Root paths come from the build entry point, from `exec` mount keys,
> and from path literals such as `@src` and `@(out/{name}.o)`.
> Sub-paths are built with the `/` operator or with interpolated path
> literals.
>
> In `exec`, Paths in `args` and `env` are resolved to VFS locations
> directly. The executor rewrites `/` separators as needed for the host
> platform when launching the process. `Path` is a locator only — reads
> through `#fs` or through `Tree` operations create dependencies and
> yield `Blob` or `Tree` values. Bare `Path` values do not carry store
> identity and are not themselves cross-boundary content. See
> [exec.mount].
>
> Interpolated path literals produce a plain `Path` when every
> interpolated value is `String` or `Path`. If an interpolated path
> literal mentions a `Tree`, `Blob`, or `ExecPath`, the result is an
> `ExecPath` instead; see [exec.arg.interpolation].

## Exec

> r[exec.signature]
>
> `exec` runs an external process on an executor — a sandboxed runner
> with a virtual filesystem. `exec` is a builtin function available in
> the standard environment, not a reserved keyword or special form. It
> uses ordinary call syntax and can be shadowed by user bindings. It is
> generic over `T`.
>
> The orchestrator does not run the process or inspect the sandbox
> directly. It serializes the full `exec` payload, including the
> observer and verifier closures, and ships that payload to an executor.
> The executor runs the process, evaluates those closures, and returns
> only the observer's produced values back across the boundary.
>
> ```vixen
> fn exec[T](
>   args: Array[ExecInput]
> ) where {
>   env: Map[String, ExecInput] = {},
>   mounts: Map[Path, Blob | Tree] = {},
>   writable: Array[Path] = [],
>   executor: ExecutorClass = executor_class "default",
>   verifier: Fn(Host) #host -> () = (_host) => (),
>   observer: Fn(Process, Tree) #sandbox -> Stream[T],
> } -> Stream[T]
> ```

> r[exec.arg]
>
> `ExecInput` is the surface type accepted by `exec`:
>
> ```vixen
> type ExecInput = String | Path | Blob | Tree | ExecPath
> ```
>
> `exec` normalizes each input into an `ExecArg` plus an implicit mount
> set:
>
> ```vixen
> type ExecArg = String | Path | ExecPath
> ```
>
> A plain `String` is a literal argument. A plain `Path` names a VFS
> location directly. `Blob`, `Tree`, and `ExecPath` carry implicit mount
> obligations.

> r[exec.arg.path]
>
> `ExecPath` is a provenance-bearing path used only by `exec`-facing
> APIs. Ordinary path operations use `Path`; `ExecPath` exists to carry
> hidden "mount this content first" information across normalization:
>
> ```vixen
> enum ExecPath {
>   struct InTree {
>     tree: Tree,
>     suffix: Path,
>   },
>
>   struct InBlob {
>     blob: Blob,
>     leaf: Path,
>   },
> }
> ```
>
> These variants are explanatory. User code usually creates them by
> passing a `Tree` or `Blob` directly to `exec`, or by using an
> interpolated path literal that mentions mounted content.

> r[exec.arg.auto-mount]
>
> Passing a `Tree` directly to `exec` is sugar for:
>
> ```vixen
> InTree { tree: some_tree, suffix: /* empty relative path */ }
> ```
>
> `exec` mounts the tree at a synthetic VFS root chosen by the runtime
> and passes that mount root path to the process. Passing a `Blob`
> directly is sugar for:
>
> ```vixen
> InBlob { blob: some_blob, leaf: @blob-<short-hash> }
> ```
>
> The blob is mounted as a single-file tree and the process receives the
> path to that file. If a blob originated from a named tree entry, that
> leaf name is reused; otherwise the runtime synthesizes a deterministic
> leaf name from the blob's content hash.

> r[exec.arg.interpolation]
>
> Interpolated path literals can preserve mount provenance:
>
> ```vixen
> @({tree}/include/lua.h)
> // means: InTree { tree, suffix: @include/lua.h }
> ```
>
> If all interpolated values are `String` or `Path`, the result is a
> plain `Path`. If exactly one interpolated value is `Tree`, `Blob`, or
> `ExecPath`, the result is an `ExecPath` rooted in that mounted content.
> Interpolating more than one mount-bearing value into the same path is
> a type error. Appending further path segments to a blob-backed path is
> also a type error — blobs are files, not directories.

> r[exec.arg.spread]
>
> In an `exec` argument array, spreading a `Tree` with `..tree`
> expands to one argument per leaf blob in that tree, recursively, in
> canonical path order. Directories do not become arguments. All
> expanded arguments share the same implicit tree mount. Ordinary array
> spread rules still apply for `..array`.

> r[exec.mount]
>
> The sandbox mount set is the union of:
>
> - explicit `mounts`, whose VFS locations are chosen by the caller
> - implicit mounts induced by `Blob`, `Tree`, and `ExecPath` inputs
>
> Explicit mounts map VFS paths (`Path`) to store content (`Blob` or
> `Tree`). The content is available at the specified path in the sandbox
> filesystem. All explicit mount paths are relative. Mounts are read-only
> by default.
>
> Every plain `Path` that appears in `args` or `env` must be covered by
> an explicit mount or writable root. `ExecPath` values carry their own
> mount obligations and are satisfied by implicit mounts. Repeated
> references to the same `Tree` or `Blob` within one `exec` are mounted
> once and reused. The actual path string seen by the process for an
> implicit mount is the runtime-assigned synthetic mount root plus the
> recorded suffix or leaf. If user code needs a stable, user-chosen VFS
> path, it must use explicit `mounts`.

> r[exec.writable]
>
> `writable` lists VFS paths (`Path`) where the process is allowed to
> write. These are empty directories created before the process starts.
> The process can create files and subdirectories within them. The
> observer can read written content from the VFS tree after the process
> runs. Writable paths are scratch space — they have nothing to do
> with store types.

> r[exec.process]
>
> `Process` has `stdout: Stream[Blob]`, `stderr: Stream[Blob]`, and
> `exit_code: U64` (pending until the process exits). No stdin — processes
> are not interactive. No signal API. `Process` exists on the executor;
> the orchestrator observes it only through the shipped observer's
> return stream.
>
> A nonzero exit code is not a runtime failure by itself. The process
> still terminates, the observer still runs, and user code decides what
> that exit status means.

> r[exec.observer]
>
> The observer runs on the executor, not the orchestrator. It is
> serialized along with its captured values and shipped to the executor.
> A closure cannot be sent until all its captures are resolved.
>
> Serialization preserves the observer's code identity, lowered body,
> and resolved captures. The executor evaluates that payload with the
> same language semantics as the orchestrator. The observer receives a
> `Process` and the VFS root as a `Tree`, and returns `Stream[T]`.
>
> The observer runs under `#sandbox`. It may inspect the process result
> and sandbox contents exposed through those arguments, but it has no
> ambient executor-host authority. In particular, an observer cannot
> inspect the unsandboxed host environment unless such data was already
> captured into ordinary values before shipping.

> r[exec.observer.materialization]
>
> When the observer produces a value back to the orchestrator, any VFS
> content referenced by that value is committed to the
> content-addressable store automatically. `Blob` and `Tree` values
> backed by the executor's sandbox are materialized into store objects
> and cross the boundary as store-backed values rather than
> executor-local filesystem handles. Bare `Path` values are locators only
> and do not carry content across the boundary. The runtime handles this
> data movement so the executor can be released.

> r[exec.observer.streaming]
>
> Streaming observers parse process output incrementally and produce
> events as artifacts become available. Downstream dependents can start
> work early — e.g., compiling a dependent crate as soon as metadata is
> emitted, without waiting for code generation.

> r[exec.observer.simple]
>
> Simple observers reference `exit_code` and VFS paths for output.
> Everything resolves when the process finishes, even if the process
> exits nonzero.

> r[exec.errors]
>
> If the executor cannot start the process, loses the process entirely,
> or otherwise fails before it can hand `Process` and `Tree` to the
> observer, the stream terminates and pending values derived from that
> stream become poisoned.
>
> By contrast, ordinary process failure is observer-visible data. A
> process that exits nonzero, is terminated by the OS, or writes error
> output still produces a `Process`; the observer interprets that result.
> Application-level errors are the observer's responsibility — parse
> stdout/stderr and call `fail`.

> r[exec.verifier]
>
> The `verifier` parameter is an optional closure that runs on the
> executor in parallel with the actual process. It checks that the
> host environment matches what was discovered — hashing binaries,
> checking paths, verifying versions. If the verifier poisons, the
> `exec` result poisons with a **verification failure**, a distinct
> poison kind that signals the executor's environment has drifted from
> the recorded discovery.
>
> Like the observer, the verifier is serialized as code plus resolved
> captures and evaluated on the executor with ordinary language
> semantics. It is not a special host-side callback. The verifier runs
> under `#host` and receives a `Host` handle representing the concrete
> executor host instance that is about to run the process.
>
> Verification does not gate execution. The process runs speculatively
> while verification proceeds concurrently. If verification fails after
> the process completes, the result is discarded and the exec poisons.

> r[exec.verifier.host]
>
> `Host` is an opaque, executor-local, read-only view of the unsandboxed
> machine. It exists only inside verifier callbacks. Host paths are
> ordinary `String` values, not `Path`, because they refer to the
> executor's real filesystem rather than to Vixen's VFS namespace.
>
> ```vixen
> namespace Host {
>   fn which(self, name: String) -> String?;
>   fn read(self, path: String) -> Blob;
>   fn hash(self, path: String) -> Blob;
> }
> ```
>
> These operations are read-only and deterministic with respect to the
> concrete executor instance. They are for re-checking discovered facts,
> not for general discovery or network access.

> r[exec.verifier.compose]
>
> `exec` accepts one verifier. If multiple environment properties must
> be checked, compose them into a single closure:
>
> ```vixen
> let verifier = (host) => (
>   check_gcc host;
>   check_ld host;
>   check_sdk_version host;
> )
>
> exec ["gcc", "-c", "main.c"] where { verifier }
> ```

> r[exec.executor-class]
>
> `executor_class(label: String) -> ExecutorClass` creates a value
> representing a class of executor environments (e.g.,
> `executor_class "ubuntu-22.04"`). The class is the unit of cache
> reuse for `exec` — cached results are keyed by executor class, not
> by specific runner instance. Different instances of the same class
> are expected to be equivalent; the verifier detects when they are
> not.

## Executor discovery

> r[exec.discover]
>
> Some toolchains cannot be acquired through `acquire` — they are
> non-redistributable (MSVC, Xcode) or must be found on the executor
> host. `discover_exec` probes an executor without sandboxing. It runs
> a command directly on the host and returns its output. It requires
> the `#discover` capability and must run inside a `stabilize` resolve
> callback.
>
> ```vixen
> fn discover_exec(
>   class: ExecutorClass,
>   args: Array[String],
> ) where {
>   env: Map[String, String] = {},
> } -> DiscoveryProcess;
> ```
>
> `DiscoveryProcess` has `stdout: Blob`, `stderr: Blob`, and
> `exit_code: U64`. There is no VFS, no mounts, no observer — the
> process sees the executor's real host filesystem.

> r[exec.discover.verifier]
>
> Discovery results are opaque to the build graph. The build does not
> branch on what version of MSVC was found — it just runs `cl.exe`.
> What discovery produces is a **verifier**: a closure that re-checks
> the same facts on the concrete executor instance running each `exec`.
>
> The verifier is constructed from discovery output by library or user
> code and passed to `exec` via the `verifier` parameter. The runtime
> does not inspect or synthesize verifiers — they are ordinary closures.
> A verifier does **not** call `discover_exec`; per-instance host checks
> go through the `Host` handle supplied to the verifier.

> r[exec.discover.reuse]
>
> Discovery results are cached by executor class through `stabilize`.
> The witness reads local project state (what toolchain is requested);
> the resolve calls `discover_exec` to learn what is actually present.
> The stabilized result — including the verifier — is reused across
> builds as long as the witness is unchanged.
>
> Per-instance verification happens at every `exec`: the verifier runs
> on the actual runner that executes the work. If verification fails
> (the runner drifted from what was discovered), the exec poisons with
> a verification failure. The cached discovery for that class is
> invalidated and rediscovery runs on retry.

> r[exec.discover.retry]
>
> A verification failure poison carries structured evidence: what was
> expected, what was observed. This distinguishes it from other poison
> kinds. The runtime or caller may retry by rerunning the `stabilize`
> witness/resolve cycle to update the cached discovery, then retrying
> the exec with the new verifier.

## Deserialization

> r[deser.parse]
>
> Vixen has built-in parsers for structured data formats. Parsing is
> driven by type — the target type's shape determines how the input is
> deserialized. Field names map to keys, `Option` fields are optional,
> sum types handle polymorphic values.

> r[deser.formats]
>
> Built-in formats:
>
> - `Toml.parse[T](blob)` — TOML.
> - `Json.parse[T](blob)` — JSON.
> - `Yaml.parse[T](blob)` — YAML.
>
> All take a `Blob` and return a value of type `T`. Parsing poisons on
> malformed input or type mismatches.

> r[deser.rename]
>
> `#[rename = "name"]` renames a single field or variant.
> `#[rename_all = "kebab-case"]` renames all fields or variants in a
> container. Supported conventions: `"snake_case"`, `"camelCase"`,
> `"PascalCase"`, `"kebab-case"`, `"SCREAMING_SNAKE_CASE"`.

> r[deser.flatten]
>
> `#[flatten]` merges a nested struct's fields into the parent during
> serialization/deserialization.

> r[deser.tag]
>
> Enum tagging modes: externally tagged (default), `#[tag = "field"]`
> for internally tagged, `#[tag = "field", content = "data"]` for
> adjacently tagged, `#[untagged]` for untagged (tries each variant in
> order).

## String methods

> r[string.test]
>
> `.starts_with(s)`, `.ends_with(s)`, `.contains(s)`, `.is_empty()` — all
> return `Bool`.

> r[string.split]
>
> `.split(sep)` returns `Array[String]`. `.split_once(sep)` returns
> `Option[(String, String)]`. `.join(sep)` on an array joins elements.
> `.lines()` splits by newline.

> r[string.transform]
>
> `.trim()`, `.trim_start()`, `.trim_end()` strip whitespace.
> `.to_upper()`, `.to_lower()` change case. `.replace(old, new)` replaces
> all occurrences. `.replace_once(old, new)` replaces the first.

> r[string.slice]
>
> `.len()` returns byte length. `.chars()` returns `Array[String]` of
> Unicode grapheme clusters. `.slice(range)` returns a substring.

> r[string.parse]
>
> `.parse_u64()`, `.parse_i64()`, `.parse_f64()` parse strings to numbers.
> They poison on invalid input. The `?` suffix returns `Option` instead.

> r[string.regex]
>
> `.matches(pattern)` tests a regex. `.find_all(pattern)` returns all
> matches. `.captures(pattern)` returns capture groups.
> `.replace_regex(pattern, replacement)` replaces by regex.

> r[string.path-ops]
>
> `.strip_prefix(s)` and `.strip_suffix(s)` return `Option[String]`.

## Evaluation model

### Demand-driven dataflow

> r[eval.demand]
>
> Vixen uses demand-driven dataflow evaluation. The runtime calls `build`
> and needs its return value — that's the demand. Everything flows backward
> from there. There is no separate planning phase.

### Concurrency

> r[eval.concurrency]
>
> There are no concurrency primitives. No `spawn`, no `async`, no `await`,
> no channels. Concurrency comes from the structure of expressions — any
> expression with multiple independent sub-expressions evaluates them
> concurrently. Function arguments, array elements, struct fields, tuple
> elements, and map values all evaluate concurrently when independent.

### Pending values

> r[eval.pending]
>
> A value is either resolved or pending. Any value of any type can be
> pending. A struct can have some fields resolved and some pending.
> Pending values can be freely passed to functions, stored in fields, put
> in arrays, and returned. None of these operations wait.

### Forcing

> r[eval.forcing]
>
> Inside the orchestrator, **nothing forces a pending value**. No
> language construct waits. `a + b` where `a` is pending produces a
> pending result. `if cond ( x ) else ( y )` where `cond` is pending
> produces a pending value. `match`, `is`, indexing, `as`, overload
> dispatch — all build nodes in the dataflow graph. None of them block.
> The language never waits.

> r[eval.forcing.boundary]
>
> Values are forced only when they cross a **boundary** between
> environments:
>
> 1. **Orchestrator → Executor** — when calling `exec`, the arguments,
>    mounts, and serialized observer/verifier payloads must be fully
>    resolved before they can be shipped to the executor process. This
>    is a serialization boundary.
> 2. **Orchestrator → External caller** — the external environment (CLI,
>    API, or other driver) demands the final result. This is where the
>    output tree must be fully materialized.
>
> These are the only two forcing points. They are both environment
> boundaries — the moment data leaves the orchestrator. Everything
> inside the orchestrator is lazy dataflow. The runtime resolves the
> graph as dependencies become available, but no language-level
> operation ever blocks waiting for a value.

### Control flow demand propagation

> r[eval.control-demand]
>
> `if`, `match`, and `is` do not force pending values. They propagate
> demand to the scrutinee/condition. Demand propagates to a branch only
> after that branch is selected by the resolved scrutinee. Branches not
> taken receive no demand — no work is done for dead branches.

### The evaluator enters functions

> r[eval.enter-functions]
>
> The evaluator does not treat functions as opaque nodes. It enters
> function bodies, discovers their dataflow graph, and schedules internal
> computations the same way it schedules top-level ones. Demand propagates
> all the way down.

### Demand depth limit

> r[eval.demand-depth]
>
> The evaluator tracks the depth of the demand chain. Two distinct
> failure modes exist:
>
> - **Circular dependency** — the evaluator detects that a value
>   transitively demands itself. This is a logic error in the program
>   and poisons with a circular dependency diagnostic.
> - **Depth exhaustion** — the demand chain exceeds a maximum depth
>   without cycling. This is a resource limit and poisons with a
>   recursion depth diagnostic. The limit is configurable.

### Memoization

> r[eval.memoization]
>
> The runtime memoizes function calls transparently. The cache key is
> **(code identity, arguments)** — the function's transitive code
> identity (see [fn.identity]) plus its resolved argument values.
> Because all values are hashable and comparable, any function call can
> be memoized. This is what makes incremental builds work — and because
> code identity is stable across comment/formatting edits, the cache
> survives source changes that do not affect behavior.
>
> `exec` cache keys are **(observer code identity, observer captures,
> args, env, mounts, executor class)**. The executor class is the unit
> of cache reuse — two `exec` calls with the same inputs and the same
> executor class may share a cached result. Because the observer's code
> identity is transitive, changing a helper function that the observer
> calls invalidates the `exec` cache — even though the observer's own
> AST did not change.

> r[eval.memoization.pending]
>
> Only fully resolved values participate in memoization. Pending values
> are not hashable for cache-key purposes and do not participate in
> memo table lookup. A function call is memoized only once all its
> arguments have resolved.

## Demand model

> r[eval.entry]
>
> A Vixen build starts with a `build.vx` file at the repository root.
> The entry point is invoked by the `build` intrinsic, which grants the
> `#fs` capability. The entry point receives the workspace path as a
> `Path`, CLI arguments as `Array[String]`, and the environment as
> `Map[String, String]`, and returns a `Tree`.
>
> ```vixen
> fn build[T](
>   entry: Fn(Path, Array[String], Map[String, String]) #fs -> T,
> ) -> T;
> ```
>
> The workspace is not pre-walked into a `Tree` — filesystem access is
> lazy. Files are read on demand through the `#fs` capability, and those
> reads are tracked by the runtime's dependency tracking machinery.

> r[eval.entry.cargo-compat]
>
> If no `build.vx` exists and a `Cargo.toml` is found, Vixen evaluates
> as if a thin build file were present calling `std.rust.workspace`. This
> makes `vx build` a drop-in replacement for `cargo build`.

## Acquire

> r[exec.acquire]
>
> `acquire(url, sha256: hash)` fetches a resource by URL and returns a
> `Blob`. The hash is always required — the result is content-addressed
> and cached permanently. The hash is verified; a mismatch poisons.
> Because acquisition is deterministic (same URL + same hash = same
> content), `acquire` is ambient — it requires no capability and can be
> called anywhere. Fetching without a known hash is discovery, which
> goes through `stabilize` (see [stabilize.model]).

> r[exec.acquire.extract]
>
> `acquire(url, sha256: hash, extract: format)` fetches a URL and
> extracts the archive in one streaming step, returning a `Tree`.
> Supported formats: `"tar.gz"`, `"tar.xz"`, `"tar.zst"`, `"zip"`.
> Extraction streams directly to the content-addressable store — the
> `Tree` is pending until extraction completes, and the entire
> operation (fetch + extract) is a single cached unit keyed by URL and
> hash.

## Capabilities

> r[cap.model]
>
> Every closure and function has a **capability fingerprint** — the set
> of capabilities it can exercise, inferred by the evaluator from the
> operations it performs and the authority-bearing values it captures.
> A function has zero capabilities by default. Capabilities are acquired
> by capturing authority-bearing values from a scope where a capability
> is active, or by being invoked through an intrinsic that grants a
> capability.

> r[cap.tags]
>
> Capability tags on `Fn` types use `#name` syntax between the parameter
> list and the return type arrow:
>
> ```vixen
> Fn() #fs -> W
> Fn(W) #discover -> T
> Fn(Process, Tree) #sandbox -> U
> Fn(Host) #host -> ()
> ```
>
> Tags are valid only in function type and function signature position.
> `#` is not an expression operator.

> r[cap.mask]
>
> Capability tags on a `Fn` type are an **upper bound** (a mask), not
> an exact requirement. A closure's inferred capability fingerprint must
> be a **subset** of the tagged set. A closure with no capabilities
> satisfies any mask. A closure with `#fs #discover` fails a mask that
> only allows `#fs`.
>
> A `Fn` type with no capability tags accepts only closures with an
> empty fingerprint — pure functions.

> r[cap.kinds]
>
> Four capabilities exist:
>
> - **`#fs`** — filesystem access. Reading files, walking directories,
>   evaluating globs. Operates on local/project state on the
>   orchestrator or build host, and is cheap to check between builds.
> - **`#discover`** — volatile external state. Registry queries,
>   toolchain channel resolution, latest-version lookups. Operates on
>   state that may differ between builds and is expensive to verify.
> - **`#sandbox`** — executor sandbox access. Reading process outputs
>   and VFS-backed `Blob`/`Tree` values produced by a running `exec`.
>   This authority is confined to the sandbox namespace for that exec.
> - **`#host`** — executor host introspection outside the sandbox.
>   Checking installed tools, reading host files, hashing binaries,
>   resolving executables on the concrete runner instance. `#host` is
>   read-only and does not imply `#discover`.

> r[cap.propagation]
>
> Capability fingerprints propagate through closures: if a closure
> captures an authority-bearing value (a handle that can exercise a
> capability), the closure carries that capability in its fingerprint.
> Calling a function that requires a capability also propagates that
> capability to the caller.
>
> Propagation is about **authority**, not data provenance. A `String`
> computed inside a `#discover` context is an ordinary `String` — capturing
> it does not make another closure carry `#discover`. Only values that
> can actually exercise authority (perform filesystem operations, issue
> discovery requests, inspect sandbox state, inspect the executor host)
> propagate capabilities. In particular, the sandbox `Tree` passed to an
> `exec` observer and the `Host` handle passed to an `exec` verifier are
> authority-bearing values.

> r[cap.grant]
>
> User code can mention capability tags in function signatures — this
> restricts what callbacks are accepted (masking). User code **cannot
> implement** functions that grant capabilities. Only runtime intrinsics
> can invoke a callback in a capability context the caller did not
> already have.
>
> This means `build` (which grants `#fs`), `stabilize` (which grants
> `#discover`), and `exec` (which grants `#sandbox` to observers and
> `#host` to verifiers) have signatures expressible in Vixen but bodies
> that only the runtime can provide. The evaluator rejects user code
> that attempts to implement a capability-granting function.

## Stabilize

> r[stabilize.model]
>
> Some operations consult changing external state — resolving a toolchain
> channel, querying a registry, performing dependency resolution. These
> are not reproducible by themselves. `stabilize` bridges discovery and
> reproducible reuse by splitting such computations into two callbacks
> with different capability masks: a **witness** that captures the
> dependency frontier, and a **resolve** that performs discovery.

> r[stabilize.signature]
>
> ```vixen
> fn stabilize[W, T](
>   witness: Fn() #fs -> W,
>   resolve: Fn(W) #discover -> T,
> ) -> T;
> ```
>
> The witness callback runs under `#fs` — it can read the filesystem but
> cannot perform discovery. The resolve callback runs under `#discover` —
> it can query external state but cannot access the filesystem. All
> local/project state the resolve callback needs must be summarized in
> the witness value it receives.

> r[stabilize.witness]
>
> The witness callback computes the dependency frontier — the set of
> local/project state that determines whether a previous resolution is
> still valid. It reads manifests, walks directories, checks globs,
> and returns a value that summarizes everything relevant. Its
> dependencies are tracked by the same holistic machinery as any other
> Vixen computation: code identity, captures, transitive calls, and all
> values read. No special tracking rules are introduced for the witness.

> r[stabilize.resolve]
>
> The resolve callback performs discovery. It receives the witness value
> and computes the final result — querying registries, resolving
> dependency graphs, looking up versions. It cannot access the
> filesystem; the `#discover` mask excludes `#fs`. This prevents
> untracked filesystem reads from causing over-caching.

> r[stabilize.reuse]
>
> The witness value is the reuse key. If the current witness matches
> the recorded witness from a previous build, the previous resolve
> result is reused without running resolve again. If the witness
> changes, resolve runs again. The witness execution trace — not a
> user-supplied key — determines reuse.

> r[stabilize.caching-bias]
>
> Over-caching — reusing a stale result — must not happen. Under-caching
> — recomputing when reuse was safe — is acceptable. The capability
> split ensures correctness: the witness captures everything that
> determines validity, and the resolve callback cannot introduce
> untracked inputs.

> r[stabilize.lockfile]
>
> Lockfiles are ordinary artifacts. They are outputs of `stabilize` and
> later inputs to reproducible builds. Evaluation never mutates
> workspace files in place — a lockfile artifact is written to disk
> only at the external boundary, like any other output.
>
> "Normal build" vs "update lockfile" is not a semantic distinction.
> The evaluator always computes the correct stabilized result. The
> difference is whether the resulting artifact gets written back to the
> repository.

> r[stabilize.compat]
>
> Ecosystem lockfiles (e.g. `Cargo.lock`) retain their native format —
> `stabilize` does not own any particular lockfile syntax. Library code
> provides package-manager-specific parsing and writing logic. Vixen
> keeps its own reuse metadata (witness fingerprints, dependency traces)
> separately.

> r[stabilize.first-build]
>
> On first evaluation of an existing project, `stabilize` runs the full
> witness/resolve cycle and records its own reuse data. Any existing
> ecosystem lockfile (e.g. a `Cargo.lock` written by `cargo`) may be
> consumed by the resolve callback as an ordinary input, according to
> that callback's logic.

> r[stabilize.nested]
>
> `stabilize` can appear anywhere in the build graph, including inside
> a resolve callback. Nesting is safe because each `stabilize` call
> enforces its own capability masks independently:
>
> ```vixen
> stabilize where {
>   witness: () => ( ... ),
>   resolve: (inputs) => (
>     // outer resolve has #discover, not #fs
>     stabilize where {
>       witness: () => ( ... ),
>       // inner witness: #fs granted by inner stabilize,
>       // #discover from outer resolve filtered by mask
>       resolve: (inner) => ( ... ),
>       // inner resolve: #discover granted by inner stabilize
>     }
>   ),
> }
> ```
