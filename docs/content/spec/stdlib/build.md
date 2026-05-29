+++
title = "Build and Runtime Standard Library"
insert_anchor_links = "heading"
weight = 10
+++

# Vixen Build and Runtime Standard Library

This chapter covers standard-library and runtime-backed surfaces that Vixen
programs can use directly. These are part of the user-visible programming
model, but they are not part of the core language syntax.

## Demand model

> r[eval.entry]
>
> A Vixen package that ships a `vx`-level subcommand exports a function
> named `main`. `vx` invokes `main` directly — there is no wrapper
> intrinsic. `main` receives the workspace as a `Tree` (content-addressed,
> ingested by `vx` before the call), pulls the standard CLI kwargs via the
> spread `with { ..VixenMain }` (`args: Array[String]`,
> `env: Map[String, String]`), and returns a `Tree`.
>
> ```vixen
> fn main(workspace: Tree) with { ..VixenMain } -> Tree (
>   Tree { "target/release" => exec [stable.rustc, workspace "src/main.rs", "-o", "out/app"] }
> )
> ```
>
> The workspace is a `Tree`, not a host path — Vixen build code never sees
> disk paths. Files are obtained through `Tree` operations
> (`workspace "src/main.rs"`), which are pure and content-addressed.
> Reading host bytes outside an ingested tree requires a `Host` capability,
> which `main` is never handed. The returned `Tree` carries
> workspace-relative paths and overlays onto the customer's workspace.

> r[eval.entry.cargo-compat]
>
> `vx cargo build` invokes `@vixen/cargo::main`. If the workspace pins no
> Vixen entry file, `vx` uses the runtime-bundled default packages (at
> minimum `@vixen/cargo`) at the versions paired with this `vx` install;
> if it pins one, `vx` uses the package versions that file names. This
> makes `vx cargo build` a drop-in for `cargo build` while keeping build
> behaviour pinnable.

## Acquire

> r[exec.acquire]
>
> `acquire url with { digest: digest }` fetches a resource by URL and
> returns a `Blob`. The digest is written as a scheme-qualified `String`
> such as `sha256:...` or `blake3:...`. A digest is always required —
> the result is content-addressed and cached permanently. The digest is
> verified; a mismatch poisons. Because acquisition is deterministic
> (same URL + same digest = same content), `acquire` needs no capability
> and can be called anywhere. Fetching without a known digest is
> discovery — it goes through the `Host` capability surface, not
> `acquire`.

> r[exec.acquire.extract]
>
> `acquire url with { digest: digest, extract: format }` fetches a URL
> and extracts the archive in one cached step, returning a `Tree`.
> `format` names the archive and compression scheme, for example
> `"tar.gz"` or `"tar.xz"`. Extraction streams directly to the
> content-addressable store — the `Tree` is pending until extraction
> completes, and the entire operation (fetch + extract) is a single
> cached unit keyed by URL, digest, and extract format.

> r[exec.render]
>
> `render template with { bindings: value }` renders a text template
> into a `Blob`. This is the primitive used for generated config
> headers, pkg-config files, generated helper scripts, and similar
> small build artifacts.
>
> The template is addressed by `Path` and usually comes from the local
> workspace or an acquired source tree. `bindings` is any structured
> value whose fields are visible to the template renderer. The exact
> template language is intentionally left unspecified here; the build
> model only requires deterministic rendering from template plus
> bindings to `Blob`.
>
> This is the main primitive needed for direct translations of
> build-system generated headers such as `zconf.h` and `pnglibconf.h`.
> Source selection, compiler/linker probes, optional outputs, and
> install-tree assembly map directly to ordinary Vixen control flow,
> discovery, and `exec`.


## Store types

> r[store.blob]
>
> `Blob` is a single file in the content-addressable store (keyed by
> Blake3 hashes). `Blob(s)` creates a blob from a text string. Blobs
> with binary content come from `exec`. A blob also carries filesystem
> mode information relevant to materialization, such as whether it
> should be executable. `blob.executable()` returns a blob whose
> materialized file mode has the executable bit set.

> r[store.bytes]
>
> `Bytes` is a raw finite byte sequence. Unlike `Blob`, it is not
> inherently file-like and carries no path or store location by itself.
> Process pipes and similar in-memory results use `Bytes`; `bytes.blob()`
> materializes them as a `Blob` when file semantics are needed.

> r[store.bytes.utf8]
>
> `bytes.utf8()` returns the byte sequence interpreted as a UTF-8
> string. It poisons if the content is not valid UTF-8.
> `bytes.utf8_lossy()` always succeeds and replaces invalid sequences.

> r[store.blob.utf8]
>
> `blob.utf8()` returns the blob's content interpreted as a UTF-8 string.
> It poisons if the content is not valid UTF-8. `blob.utf8_lossy()`
> always succeeds and replaces invalid sequences.

> r[store.tree]
>
> `Tree` is a directory with its full structure and filenames preserved.
> There is no separate "local tree" vs "store tree" type — the runtime
> handles materialization transparently.
>
> `Tree(map)` creates a tree from a map of paths to entries. In authored
> code this is usually written as `Tree %{ ... }`.
> `Tree(path)` creates a lazy tree handle rooted at a local
> filesystem path.
> `Tree %{}` is the empty tree.
> `tree.subtree(path)` selects a subtree. `tree.glob(pattern)` filters by
> glob. `tree.merge(other)` merges two trees (later entries win). These
> operations are lazy.

> r[store.tree.file]
>
> `tree.file(path)` returns the `Blob` at the given relative path within
> the tree, and **poisons** if the path does not exist or points to a
> directory. `tree.find(path)` is the recoverable form: it returns
> `Option[Blob]` — `None` if the path is absent (still poisons if the
> path resolves to a directory).

> r[store.path]
>
> Path-shaped values are ordinary strings. There is no distinct
> path-literal syntax: in `exec [rustc, workspace "src/main.rs", "-o",
> "out/app"]` all four literals are plain strings. What distinguishes a
> "path" from "text" is **provenance**, not quoting — the runtime tracks
> which strings originated as `Tree` projections (or were derived from
> them) and uses that provenance for cache keying and implicit sandbox
> mounting.
>
> `Path` is the type a path-shaped value carries when its provenance is
> known; it is a normalized slash-delimited string. `Path(s)` parses and
> normalizes a string, poisoning if it is not a valid normalized path;
> `path.to_string()` recovers the spelling; `path.join`, `path.split`, and
> similar live on the value type. Sub-paths are built with the `/`
> operator or with backtick interpolation (`` `out/${name}.o` ``).
> Provenance is preserved through `Path` joins and lost through arbitrary
> string transforms the runtime can't track — the result is then a plain
> `String`.
>
> In `exec`, path-shaped values in `args` and `env` are resolved to VFS
> locations directly. The executor rewrites `/` separators as needed for
> the host platform when launching the process. A path is a locator only —
> reads through `Tree` operations (or, inside a probe, through the `Host`
> capability) create dependencies and yield `Blob` or `Tree` values. Plain
> path strings do not carry store identity and are not themselves
> cross-boundary content; provenance-bearing values (`Tree` / `Blob`
> projections, `ExecPath`) do. See [exec.mount] and
> [exec.arg.interpolation].

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
> observer closure and any discovery-derived verification obligations
> referenced by the inputs, and ships that payload to an executor. The
> executor runs the process, evaluates the observer there, rechecks the
> relevant host obligations on the concrete runner instance, and returns
> only the observer's produced values back across the boundary.
>
> ```vixen
> fn exec[T](
>   args: Array[ExecInput]
> ) with {
>   env: Map[String, ExecInput] = %{},
>   mounts: Map[Path, TreeEntry] = %{},
>   coarse_mounts: Array[SandboxExemption] = [],
>   writable: Array[Path] = [],
>   create_dirs: Array[Path] = [],
>   observer: Fn(Process, Tree) -> T,
> } -> T
> ```
>
> There is no `executor:` kwarg and no effect annotation on the observer.
> Routing is type-driven: the runtime chooses an executor from the ability
> values (e.g. a `RustToolchain`) that flow into `args`, by predicate
> match — there is no stringly-typed executor selector.

> r[exec.arg]
>
> `ExecInput` is the surface type accepted by `exec`:
>
> ```vixen
> type ExecInput = String | Path | Blob | Tree | ExecPath
> ```
>
> Explicit tree-shaped payloads such as VFS mounts use:
>
> ```vixen
> type TreeEntry = Blob | Tree
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

> r[exec.coarse-mounts]
>
> `coarse_mounts` is the explicit surface for host-backed sandbox
> exemptions that cannot be justified through ordinary VFS tracking.
> Unlike `mounts`, which are fine-grained VFS content exposures, coarse
> mounts are host filesystem holes chosen by authored Vixen code and
> verified by the runtime on the concrete runner.
>
> ```vixen
> enum SandboxExemption {
>   struct ExecSubpath {
>     root: String,
>   },
>
>   struct ReadSubpath {
>     root: String,
>   },
> }
> ```
>
> Both variants are **read-only**: a coarse exemption never grants write
> access to the host filesystem. `ReadSubpath` permits reads under `root`;
> `ExecSubpath` permits read and execute (running a discovered tool) but
> never write. Host state is an input to a build, never a sink — nothing
> the sandboxed process does can mutate a coarse-mounted host path. The
> runtime applies these exemptions only after rechecking the discovery
> observations attached to them on the concrete runner.

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
> passing a `Tree` or `Blob` directly to `exec`, or by projecting into a
> tree (`tree "include/lua.h"`), whose result carries the tree's mount
> provenance.

> r[exec.arg.auto-mount]
>
> Passing a `Tree` directly to `exec` is sugar for:
>
> ```vixen
> InTree { tree: some_tree, suffix: /* empty relative path */ }
> ```
>
> `exec` mounts the tree at a synthetic VFS root chosen by the runtime.
> If the tree contains exactly one file entry, `exec` passes that file's
> path to the process directly. Otherwise it passes the mount root path.
> This makes singleton output trees convenient to feed into later `exec`
> calls without extra path extraction, while keeping plural trees explicit.
> Passing a `Blob`
> directly is sugar for:
>
> ```vixen
> InBlob { blob: some_blob, leaf: "blob-<short-hash>" }
> ```
>
> The blob is mounted as a single-file tree and the process receives the
> path to that file. If a blob originated from a named tree entry, that
> leaf name is reused; otherwise the runtime synthesizes a deterministic
> leaf name from the blob's content hash.

> r[exec.arg.interpolation]
>
> Mount provenance is carried by projecting a `Tree`, not by a special
> literal. `tree "include/lua.h"` returns an `ExecPath` rooted in that
> tree's mounted content — equivalent to
> `InTree { tree, suffix: "include/lua.h" }`. Backtick interpolation
> builds plain path strings:
>
> ```vixen
> tree "include/lua.h"          // ExecPath, carries tree provenance
> `out/${name}.o`               // plain String, no provenance
> ```
>
> A value built purely from `String` / `Path` pieces is a plain path with
> no mount provenance. Provenance comes from a `Tree`, `Blob`, or
> `ExecPath` source; combining more than one mount-bearing source into a
> single path is a type error, and appending further segments to a
> blob-backed path is also a type error — blobs are files, not
> directories.

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
> filesystem. All explicit mount paths are relative.
>
> The sandbox filesystem is copy-on-write for the lifetime of one `exec`.
> The process may write anywhere in its VFS namespace, including paths
> backed by mounted content. Overwriting an input mutates only that
> exec-local view; it does not mutate the original `Blob` or `Tree`
> values or any shared cache entry. `ExecPath` values carry their own
> mount obligations and are satisfied by implicit mounts. Repeated
> references to the same `Tree` or `Blob` within one `exec` are mounted
> once and reused. The actual path string seen by the process for an
> implicit mount is the runtime-assigned synthetic mount root plus the
> recorded suffix or leaf. If user code needs a stable, user-chosen VFS
> path, it must use explicit `mounts`.

> r[exec.writable]
>
> `writable` declares sandbox-relative directories that should exist as
> writable roots before the process starts. `create_dirs` declares
> additional directories to create under those roots before launch.
>
> ```vixen
> exec [cc, "-c", "src/hello.c", "-o", "out/obj/hello.o"] with {
>   mounts: %{ "src" => source },
>   writable: ["out"],
>   create_dirs: ["out/obj"],
>   observer: (proc, vfs) => vfs.subtree("out"),
> }
> ```
>
> Both `writable` and `create_dirs` use relative VFS `Path` values.
> `create_dirs` entries must be equal to or nested under one of the
> writable roots.
>
> If `writable` is omitted, the runtime may infer writable roots from
> the parent directories of output `Path` arguments and environment
> values, so long as those paths are not already satisfied by mounts.
> In authored code, explicit `writable: ["out"]` is preferred because it
> makes intended outputs obvious and avoids depending on inference.

> r[exec.process]
>
> `Process` has `stdout: Stream[Bytes]`, `stderr: Stream[Bytes]`, and
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
> `Process` and the VFS root as a `Tree`, and returns `T`.
>
> The observer's authority is exactly the `Process` and sandbox `Tree` it
> receives — it may inspect the process result and sandbox contents
> exposed through those arguments, but it holds no `Host` and has no
> ambient executor-host authority. An observer cannot inspect the
> unsandboxed host environment; only data already captured into ordinary
> (content-addressable) values before shipping is available to it.

> r[exec.observer.cacheable]
>
> The cache stores the observer's `T`; for that to be sound, `T` must be a
> deterministic function of the process's **content**, not its **timing**.
> The observer may freely consume the process streams to *schedule* when
> deterministic sub-results become available — this is what build
> pipelining needs: surface a crate's `.rmeta` the instant rustc's
> metadata-artifact message arrives, so dependents start before codegen
> finishes. What it may not do is let arrival order *determine* a value.
>
> This is a static check over the observer's combinator graph
> (`r[comb.determinism]`), the same flavour as the kernel partition: the
> observer is cacheable iff no order-exposing combinator (`.next()`, a
> racing `.find`) is reachable from its return value. Order-erasing and
> unique-select (`.only()`) results are content-determined and cacheable;
> an order-exposing result reaching the return makes the exec non-cacheable.

> r[exec.tree.incremental]
>
> The sandbox write-set `Tree` an observer receives is **incrementally
> available**: an entry resolves when the process finalizes that file, not
> when the process exits. So `tree.find("out/libC.rmeta")` can resolve
> mid-execution, and a dependent exec demanding it pipelines against the
> still-running producer. Finalization is a runtime signal (file close /
> atomic rename); where that is unreliable, the package detects it from the
> process's own output — `@vixen/rust` correlates rustc's `--json`
> artifact messages to write-set entries. The entry's *value* is the
> deterministic file content either way; only its availability is early.

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
> If the observer returns `Stream[T]`, `exec` itself returns
> `Stream[T]`. Streaming observers parse process output incrementally
> and produce events as artifacts become available. Downstream
> dependents can start work early — e.g., compiling a dependent crate
> as soon as metadata is emitted, without waiting for code generation.

> r[exec.observer.simple]
>
> Simple observers reference `exit_code` and VFS paths for output.
> Everything resolves when the process finishes, even if the process
> exits nonzero.

> r[exec.errors]
>
> If the executor cannot start the process, loses the process entirely,
> or otherwise fails before it can hand `Process` and `Tree` to the
> observer, `exec` poisons. If the observer's result type is
> `Stream[T]`, that stream terminates and pending values derived from it
> become poisoned.
>
> By contrast, ordinary process failure is observer-visible data. A
> process that exits nonzero, is terminated by the OS, or writes error
> output still produces a `Process`; the observer interprets that result.
> Application-level errors are the observer's responsibility — parse
> stdout/stderr and call `fail`.

> r[exec.verifier]
>
> `exec` automatically rechecks any host obligations carried by its
> inputs on the concrete executor instance that is about to run the
> process. Verification is derived from recorded host observations; the
> normal user-facing model does not include a handwritten verifier
> closure.
>
> These obligations live on values. If a capability probe observed
> `host.which("cc")` and `host.hash(cc_path)`, then the probed tool value
> carries those facts with it. Destructuring or embedding that value does
> not erase its obligations.
>
> Verification does not gate execution. The process runs speculatively
> while verification proceeds concurrently. If verification fails after
> the process completes, the result is discarded and the exec poisons
> with a **verification failure**, a distinct poison kind that signals
> the executor's environment has drifted from what the probe recorded.

> r[exec.verifier.host]
>
> `Host` is an opaque, executor-local, read-only view of the unsandboxed
> machine, handed to a capability probe (see [discover.probe]). It is a
> non-constructible capability value (see [cap.types]), region-scoped to
> the probe (see [cap.region]). Host paths are ordinary `String` values,
> not `Path`, because they refer to the executor's real filesystem rather
> than to Vixen's VFS namespace.
>
> ```vixen
> namespace Host {
>   fn which(self, name: String) -> Option[String];
>   fn read(self, path: String) -> Blob;
>   fn hash(self, path: String) -> Blob;
>   fn exec(self, args: Array[String]) -> HostOutput;
>   fn exec_subpath(self, root: String) -> SandboxExemption;
>   fn read_subpath(self, root: String) -> SandboxExemption;
> }
> ```
>
> These operations are read-only and deterministic with respect to the
> concrete executor instance. `host.which(name)` returns `Option[String]`
> — `None` if the tool isn't found, since probing for presence is
> inherently recoverable. `host.hash(path)` uses Blake3 and records the
> observed host fact. `host.exec(args)` runs a tool and returns a small
> `HostOutput` (captured output bytes and exit code) — deliberately
> narrower than the observer's `Process`, to keep the recorded obligation
> tuple small. `host.exec_subpath(root)` and `host.read_subpath(root)`
> produce explicit coarse sandbox exemptions to be carried out of a probe
> and later passed back into `exec` through `coarse_mounts`. Every such
> operation is recorded as an obligation on the values that flow out of
> the probe. These operations are for probing and rechecking probed
> facts, not for general network access.

> r[exec.verifier.host.coarse]
>
> Host observations are intentionally weaker than ordinary VFS-backed
> `exec` inputs. A `Tree`, `Blob`, or `ExecPath` mounted into the sandbox
> is fine-grained runtime-visible content: reads and writes happen in the
> VFS model, and dependency/provenance tracking can follow that content
> precisely. By contrast, host paths discovered through `Host` are coarse
> host facts: they refer to the executor's real filesystem outside the
> VFS, and the runtime cannot in general track every file read that later
> host-side execution may perform beneath such a path.
>
> Coarse host facts are therefore justified by probing and verification,
> not by ordinary VFS tracking. Authored Vixen code must carry explicit
> `SandboxExemption` values out of a probe and pass them back into
> `exec` through `coarse_mounts`; the runtime does not invent coarse host
> policy on behalf of the build. When a discovered exemption later
> authorizes host-side access during `exec`, that access must be
> explained by the carried observations attached to the exemption and
> rechecked on the concrete runner. Implementations should apply only the
> explicitly requested verified exemptions rather than granting broad
> host roots unconditionally.

> r[exec.verifier.compose]
>
> When a probe performs multiple host observations, the returned value
> carries the union of the obligations attached to the values that flow
> into that result. There is no separate user-written composition step —
> obligation tracking follows the values the probe produces.
>
> Observations from expressions that do not flow into the returned value
> do not contribute obligations. Such unused expression statements are a
> static error.

## Executor discovery

> r[discover.probe]
>
> Toolchains that cannot be acquired with a digest — non-redistributable
> ones (MSVC, Xcode) or whatever is installed on an executor host — are
> found by **capability probes**. There is no user-callable `discover`
> intrinsic. A probe is a function declared in a capability-probe package,
> by convention in the advertised type's namespace, taking a `Host` and
> returning `Option[T]`:
>
> ```vixen
> struct MacToolchain { xcode: SemVer, macos_sdk: SemVer, arch: Arch }
>
> namespace MacToolchain {
>   fn probe(host: Host) -> Option[MacToolchain] ( ... )
> }
> ```
>
> The package manifest declares what it advertises:
>
> ```styx
> capability { advertises MacToolchain }
> ```

> r[discover.daemon]
>
> A `vx-daemon` walks its configured probe list at startup, loads each
> package, calls each `probe` with a `Host` for the daemon's own machine,
> and publishes the `Some(T)` results to the fleet registry over Vox. A
> probe that doesn't apply on the host (a Mac probe on Linux) returns
> `None` and advertises nothing. Build code never invokes a probe: it
> requests abilities structurally with `Type where (...)`, and the
> orchestrator matches the request against advertised values. Probe
> lifecycle, advertisement, and routing are specified by the
> executor-abilities model.

> r[discover.obligations]
>
> While a probe runs, the runtime records the host facts it consulted
> (`host.which`, `host.read`, `host.hash`, `host.exec`). Those
> observations become verification obligations attached to the values that
> flow out of the probe (see [exec.verifier], [exec.verifier.compose]).
> The `Option`-returning forms preserve them: if `host.which(name)`
> yields `None`, that `Option` still carries the recorded
> `Which { expected_path: None }` obligation. Reuse across builds is
> governed by those obligations and verification-races-with-work, not by a
> separate persistence primitive: the runtime rechecks the relevant
> obligations on the actual runner at every `exec`, and drift poisons with
> a verification failure carrying structured expected-vs-observed evidence.

> r[discover.http]
>
> `Http.get(url) -> Blob` performs an ordinary HTTP GET and returns the
> response body. It is the low-level primitive for registry index queries,
> metadata fetches, and other volatile network reads. Vixen provides no
> ecosystem-specific registry builtins in the core model; package
> resolution is ordinary code over `Http.get`, structured parsers, and the
> resolver intrinsic (see [resolve]). Because the result is volatile,
> `Http.get` is not a from-cache pure value the way `acquire` is; its use
> in resolution is demand-gated through the lockfile, so registry drift
> does not re-resolve a still-valid lock.


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
> - `Toml.parse[T] blob` — TOML.
> - `Json.parse[T] blob` — JSON.
> - `Yaml.parse[T] blob` — YAML.
>
> All take a `Blob` and return a value of type `T`. Parsing poisons on
> malformed input or type mismatches.

> r[deser.serialize]
>
> Structured values can also be serialized back out:
>
> - `Toml.serialize value` — TOML.
>
> This is the primitive used for writing lockfiles such as
> `Cargo.lock`, generated manifests, and other structured build
> artifacts. The current core model only guarantees TOML serialization;
> other formats may be added later.

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
> `s.starts_with(prefix)`, `s.ends_with(suffix)`, `s.contains(needle)`, and
> `s.is_empty()` all return `Bool`.

> r[string.split]
>
> `s.split(sep)` returns `Array[String]`. `s.split_once(sep)` returns
> `Option[(String, String)]`. `parts.join(sep)` on an array joins
> elements. `s.lines()` splits by newline.

> r[string.transform]
>
> `s.trim()`, `s.trim_start()`, and `s.trim_end()` strip whitespace.
> `s.to_upper()` and `s.to_lower()` change case. `s.replace(old, new)`
> replaces all occurrences. `s.replace_once(old, new)` replaces the
> first.

> r[string.slice]
>
> `s.len()` returns byte length. `s.chars()` returns `Array[String]` of
> Unicode grapheme clusters. `s.slice(range)` returns a substring.

> r[string.parse]
>
> `s.parse_u64()`, `s.parse_i64()`, and `s.parse_f64()` parse strings to
> numbers, returning `Option` — `None` on invalid input, since parsing
> external text is a recoverable outcome to branch on. Use `.unwrap()`
> (poisons on `None`) where a valid number is required.

> r[string.regex]
>
> `s.matches(pattern)` tests a regex. `s.find_all(pattern)` returns all
> matches. `s.captures(pattern)` returns capture groups.
> `s.replace_regex (pattern, replacement)` replaces by regex.

> r[string.path-ops]

## Capabilities

> r[cap.model]
>
> Capabilities are conveyed by **parameter types**, object-capability
> style. There is no effect-annotation system — no `#fs` / `#discover` /
> `#sandbox` / `#host` tags, no capability fingerprints, no masks. A
> function may perform an authority-bearing operation if and only if it
> holds a value of a capability-bearing type, received as a parameter.
> Whoever holds such a value has the runtime's permission to do what that
> type allows; whoever does not, cannot. The type system already tracks
> where a capability value flows, so nothing else is needed.

> r[cap.types]
>
> The capability-bearing types are:
>
> - **`Host`** — a read-only view of an executor's unsandboxed machine,
>   handed to a capability probe. Methods such as `host.which`,
>   `host.read`, `host.hash` (see [exec.verifier.host]).
> - **`Process`** and the **sandbox-output `Tree`** — handed to an `exec`
>   observer. The observer reads the process result (stdout / stderr /
>   exit code) and the write-set tree.
>
> These types are **non-constructible by user code** — there is no public
> constructor. Only the runtime produces them, by passing them into the
> callback that needs them.

> r[cap.region]
>
> A capability value is **region-scoped** to the callback the runtime
> handed it to. User code may call methods on it, but may **not** capture
> it into a closure, return it, or store it in any value that outlives the
> callback. This is not a separate enforcement layer — it follows from two
> invariants the language already has:
>
> - Values are serialisable, and a closure cannot ship until its captures
>   are resolved (see [exec.observer]). `Host`, `Process`, and the live
>   sandbox `Tree` are not content-addressable, so they can never be a
>   closure capture.
> - A callback's return value crosses a boundary and is materialised. For
>   `Process` and the sandbox `Tree` this is graceful — VFS content in the
>   return value commits to the store and crosses as a store-backed value
>   (see [exec.observer.materialization]); the live handle dies. `Host` is
>   not content-addressable, so a return value still holding a live `Host`
>   has nothing to materialise into and is a static error.
>
> The discipline is borrow-like: capabilities are usable within their
> region; data *derived* from them (a discovered toolchain struct, a store
> `Tree`, a realised `Stream`) flows out freely; the live handle never
> escapes. Over-reach is caught at compile time, when the value you tried
> to return or capture fails to type or serialise.
>
> The check is a transitive **escape check at the minting boundary** (the
> observer call, the probe, the `accumulate` block), not a per-function
> restriction. A capability threads freely *within* its region — passed
> down to a subroutine and even returned back up to the same region
> (`fn helper(p: Process) -> Process` is a legal re-borrow). What is
> rejected is a capability — or a closure capturing one — crossing the
> minting region's boundary. The analysis is narrow: only capability types
> are region-bound; ordinary content-addressed values are unrestricted,
> and there is no shared/mutable-borrow distinction — just "these few types
> cannot escape their region."

> r[cap.grant]
>
> Because capability-bearing types are non-constructible, user code cannot
> manufacture a capability or write a function that produces one. Only
> runtime intrinsics hand out capability values: `exec` passes a `Process`
> and sandbox `Tree` to its observer; a capability probe receives a `Host`.
> Those intrinsics have signatures expressible in Vixen but bodies only the
> runtime can provide. The evaluator rejects user code that attempts to
> construct a capability value or implement such an intrinsic.

## Resolution

> r[resolve.model]
>
> Dependency resolution is **per-ecosystem domain logic** in the language
> package (`@vixen/rust`, `@vixen/go`, `@vixen/node`, …), not a single
> runtime "resolver." Ecosystems disagree about what resolution even is —
> backtracking-SAT with one version per package (cargo, Python, Dart),
> Minimum Version Selection (Go), nested trees where versions coexist
> (npm), nearest-wins mediation (Maven). The runtime supplies only the
> shared, hard primitives; the package composes them:
>
> - **Monotone-fold resolution** — Go's MVS (select the max of all required
>   minimums, to fixpoint) and cargo feature unification — is an
>   accumulator fixpoint (`r[accum.fixpoint]`, `merge = max` / `merge =
>   union`). No solver intrinsic.
> - **Backtracking-SAT resolution** — cargo, Python, Dart — uses the
>   `pubgrub_solve` intrinsic (`r[resolve.pubgrub]`).
> - **Tree-shaped resolution** — npm's nesting, Maven's mediation — is
>   plain Vixen graph / `Tree` construction. No solver intrinsic.
>
> `pubgrub_solve` is one tool for the backtracking class, **not** the
> universal resolver.

> r[resolve.pubgrub]
>
> `pubgrub_solve[P, V]` is a generic SAT-shaped constraint solver. It knows
> no registry and no ecosystem: it drives the PubGrub algorithm and calls
> back into domain-supplied closures for the package-specific facts.
>
> ```vixen
> fn pubgrub_solve[P, V](root: P) with {
>   available_versions: Fn(P) -> Array[V],            // versions of P, newest first
>   dependencies:       Fn(P, V) -> Map[P, Constraint[V]],
>   prefer:             Fn(P, Constraint[V]) -> Option[V],   // lockfile bias; None = solver picks
> } -> Resolution[P, V];
> ```
>
> `V` must be totally ordered (`SemVer` is). `P` is the package identity
> the domain chooses; ecosystem subtleties live in how the package defines
> `P` and what `dependencies` returns. A dependency requirement is an
> ordinary `Constraint[V]` (`r[constraint.model]`) — e.g. `version ^= 1.0`.

> r[resolve.pubgrub.io]
>
> The closures perform registry I/O (`Http`) lazily as the solver explores
> — the runtime evaluates them mid-solve, so only the part of the universe
> the search touches is fetched, and each `Http` lookup caches on its own.
> The *solve* caches on the **read-set it consulted**: the `(P → versions)`
> and `(P, V → deps)` facts the closures actually returned. Re-running with
> the same consulted facts yields the same result; a publish to a package
> the solve *touched* invalidates it, a publish elsewhere does not — the
> same read-set discipline as the sandbox.

> r[resolve.pubgrub.conflict]
>
> `Resolution[P, V]` is `Solved(Map[P, V])` or `Conflict(<derivation>)`. A
> conflict is a returned value carrying PubGrub's derivation tree, **not** a
> poison — the domain package formats it into an ecosystem-shaped error
> (cargo's "failed to select a version for …"). Resolution diagnostics are
> prime UX; they are not swallowed.

> r[resolve.pubgrub.prefer]
>
> `prefer(P, constraint)` is the lockfile bias: it returns the prior lock's
> version for `P` when that version satisfies `constraint` and `P` is not
> being updated, else `None` (the solver picks the highest compatible).
> This is how `cargo update` semantics live in the package, not the
> runtime: bare `update` makes `prefer` return `None` throughout;
> `update -p foo` returns the locked version for every package except
> `foo`; an ordinary re-resolution keeps every pin.

> r[resolve.lockfile]
>
> A lockfile is an ordinary workspace `Tree` file, not a runtime
> persistence mechanism — there is no `stabilize`. Stability comes from
> demand-gating: an ordinary build whose committed lock satisfies the
> manifest never forces `pubgrub_solve`, so registry drift cannot
> re-resolve it. The solver is forced only when the lock can't serve as the
> solution (missing, manifest changed, or explicit update); its output
> overlays back as the new lock. The bar is functional equivalence with the
> ecosystem's own resolver, not a byte-identical lockfile.

> r[resolve.features]
>
> Cargo-style feature unification is **not** part of the solver. A crate's
> effective feature set is the union of every feature its dependents
> request — a monotone fixpoint — computed with an accumulator
> (`r[accum.fixpoint]`, `merge = union`) running alongside the version
> solve: activating a feature can add an optional-dependency edge, which
> feeds the next solve iteration, until both stabilize. `P` stays
> `crate@version`; features ride the accumulator. cfg / target-table edges
> thread in the same way (a per-target cfg set from `rustc --print cfg`,
> evaluated against the `cfg(...)` expression in `@vixen/rust`).
