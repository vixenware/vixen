+++
title = "Epic: vx cargo build, end-to-end"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-05-26"
+++

The runtime trace from a customer typing `vx cargo build --release` to a built binary. Recorded incrementally as the design is settled. Blocking tasks identified along the way get added as separate `Task` pages and linked from the relevant section.

## Step 1: `vx` starts up

Customer types `vx cargo build --release` in a workspace. `vx` is the CLI installed by the setup-rust action (Tier 2 onwards).

The vx binary ships with a default set of packages — at minimum `@vixen/cargo` — installed alongside it at the version they were paired with at install time. A fresh `vx` install offline can still do `vx cargo build` on a workspace whose deps are all already on disk. (Will most cargo deps be cache-resident on a fresh install offline? Probably not — but the *runtime* itself doesn't fail to start, which is the bar that matters.)

## Step 2: Subcommand resolution

`vx` parses its args, sees the subcommand `cargo`. It needs to find the package that implements `cargo` and dispatch to it.

**Resolution discipline:**

- **If the workspace has no `main.vx` (or `vx.build` — file shape TBD; see [Packages and publishing](/packages-and-publishing/)):** `vx` uses the **runtime-bundled defaults**. Whatever `@vixen/cargo` shipped with this `vx` install is what runs. Default behaviour changes as `vx` updates.
- **If the workspace has a Vixen entry file:** `vx` reads it. The customer's file declares which packages are active and at what pinned versions. `vx cargo` finds `@vixen/cargo` at the pinned version.

`vx init` (mechanism, name TBD) bakes the current defaults into a fresh Vixen entry file in the workspace. After running it, the workspace is on those exact package versions; further `vx` upgrades don't change the build behaviour.

This is the durability discipline: ride the wave by default, pin if you care. Same shape as cargo's edition + lockfile.

## Step 3: Single endpoint per scope, package does its own subcommand parsing

`vx cargo …` invokes a **single endpoint** on `@vixen/cargo` — the `main` function. The package does its own internal subcommand parsing.

So `vx cargo build --release foo` invokes:

```
@vixen/cargo::main(
  args: ["build", "--release", "foo"],
  env:  { … process env, filtered/passed TBD },
  workspace: <Tree handle for the customer's workspace>,
)
```

(Signature to be refined — args / env are keyword arguments per Vixen convention.)

`main` does its own arg parsing to dispatch internally: `"build"` → its build handler; `"test"` → its test handler; etc. Vixen provides packages that help with arg parsing (a vixen-side equivalent of clap or similar — exact shape TBD).

The convention scales: any package owning a Vixen subcommand verb exports `main`. `@vixen/python::main`, `@vixen/protobuf::main` follow the same pattern. Each package owns one verb at the `vx` level; it can route arbitrarily many internal subcommands underneath.

**Why a single endpoint, not multiple per package:** simpler dispatch in `vx-the-binary`; the package controls its own UX (help text, error messages, …) without `vx` having to mediate; arg-parsing libraries can be shared via Vixen packages.

### The workspace is a Tree, not a Path

A load-bearing point: Vixen-level code never sees disk paths. The workspace handed to `main` is a `Tree` — a [Merkle-rooted content-addressed directory structure](/glossary/#subgraph-elision) of the customer's repo, ingested into the local store (chunked via CDC) by `vx` before `main` is invoked. From `main`'s perspective there's no `read_file("Cargo.toml") #fs`; there's `workspace.file("Cargo.toml") -> Blob`, a pure Tree operation. No `#fs` effect required at this level.

Why this matters:

- **The Tree doesn't have to be on disk.** Files might live as chunks in the local store, in memory, in a remote store. Tree operations don't care.
- **Remote execution is automatic.** When we ship work to a remote executor, we send the Tree *handle*. How it materialises on the executor side (lazy fetch via VFS, eager prefetch, whatever) is the runtime's concern, not the build-system code's. No explicit upload / download / tarball.
- **Caching falls out.** The Tree is content-hashed; everything that depends on it is cacheable by that hash.
- **Provenance flows through.** The Tree's identity participates in cache keys for every node downstream of it.

The `#fs` effect exists for the case where Vixen code genuinely needs to read host-side bytes outside of any ingested tree (a local-discovery probe inspecting `/usr/bin/...`, for example). For normal build code operating on workspace contents, it doesn't.

## Step 4: Parse `Cargo.toml`

`@vixen/cargo::main` now has the workspace as a `Tree`. First substantive thing: parse `Cargo.toml` into a typed `CargoManifest` value. Roughly:

```
let manifest_blob: Blob = workspace.file("Cargo.toml");
let manifest: CargoManifest = Toml::parse[CargoManifest](manifest_blob);
```

`CargoManifest` is declared on the Vixen side with the schema annotations (rename, alias, optional, …) — same pattern as facet-style typed deserialisation; runtime parses TOML bytes *into* the Vixen type using the schema.

## Step 5: Dependency resolution

`@vixen/cargo` reads the constraints from `CargoManifest`, talks to crates.io for each dep's available version list, runs the PubGrub intrinsic over the constraints, emits a resolved set (lockfile-shape).

Layering — whether the registry-protocol bits live directly inside `@vixen/cargo` or in a separate `@vixen/crates-io` library it depends on — is a code-organisation question, not a caching question. Cache semantics are identical either way: the same intrinsic calls happen with the same arguments; intermediate Vixen function calls add no overhead because pure functions over the same inputs produce the same output and the same content-addressed identity. Modular for engineering hygiene; defer the exact factoring to implementation time.

Cache key for the resolution operation is `(CargoManifest, registry view at this moment, resolver version)`. Same inputs ⇒ same lockfile ⇒ instant hit. New version published on crates.io ⇒ different registry view ⇒ miss, re-resolve. See [Build and resolution](/build-and-resolution/) for the lockfile-as-output framing.

## Step 6: Fetching dep sources

With resolution done, `@vixen/cargo` has a set of `(crate-name, version, registry-hash)` triples. To compile each, it needs the source bytes as a `Tree`. The existing `acquire` intrinsic does it:

```
let serde_src: Tree = acquire "https://crates.io/api/v1/crates/serde/1.0.197/download" with {
  digest: "blake2b:...",
  extract: "tar.gz",
}
```

Cache key for the acquire result is `(url, digest, extract)` — content-addressed, deterministic. Same dep across two builds = same key = instant.

Layering of crates.io URL construction (string interpolation of crate name + version into the download URL) is the same story as crates.io protocol layering generally: either inline in `@vixen/cargo` or in a `@vixen/crates-io` library. Doesn't affect caching.

Reminder about Vixen syntax used here:

- **Function application is positional, single-payload.** `f x` is one positional payload; use a tuple `f (x, y)` to pass what looks like multiple args. (See `docs/content/spec/language/core.md`.)
- **Juxtaposition (no parens) is allowed when the payload is self-delimiting** — string / array / record / set literals all qualify. Bare identifiers and most expressions still need parens.
- **`with { … }`** applies the record as keyword args. Not a second positional argument.

## Step 7: Compiling each unit

`@vixen/cargo` enumerates the dep graph in Vixen and emits an `exec` call per unit (per crate compile). The runtime then figures out what to evaluate, when, on which executor, how to compose results — the build code doesn't schedule, it describes.

Shape:

```
let syn_rlib: Blob = exec [
  rustc,
  "--edition=2021",
  "--crate-type=lib",
  syn_src.file "src/lib.rs",
  "-o",
  "./out/libsyn.rlib",
] with {
  observer: fn (proc, tree) -> tree.file "./out/libsyn.rlib",
}

let serde_rlib: Blob = exec [
  rustc,
  "--edition=2021",
  "--crate-type=lib",
  serde_src.file "src/lib.rs",
  "--extern", "syn=" ++ syn_rlib,            // exact string-concat syntax TBD
  "-o",
  "./out/libserde.rlib",
] with {
  observer: fn (proc, tree) -> tree.file "./out/libserde.rlib",
}
```

Things load-bearing in this shape that aren't obvious from a Rust/Bazel-shaped reading:

- **All paths are relative to `.`.** Sandbox filesystem is rooted at the working directory, not `/`. This is forced by needing to run on Windows and macOS, where mounting at the OS root isn't viable. So `./src/lib.rs`, `./out/libfoo.rlib`, not `/src/...` or `/out/...`.
- **`writable` defaults to `./out`** (or equivalent). No need to declare `writable: ["./out"]` for ordinary compiles.
- **Mounts are implicit when values are referenced as exec arguments.** Passing `rustc` (a `Blob` / `ExecPath` value coming from `rustc_toolchain`) as `args[0]` causes the runtime to materialise the toolchain in the sandbox and substitute the concrete path. Same for `syn_src.file "src/lib.rs"` (returns an `ExecPath` carrying its source-tree provenance) and `syn_rlib` (when concatenated into the `--extern` arg, the runtime materialises it and substitutes the path). The `ExecPath` type in `core.vx` is specifically "a path-like exec argument carrying implicit mount provenance." Explicit `mounts: %{ … }` is for cases the implicit machinery can't reach.

So the runtime sees the graph of exec calls produced by `@vixen/cargo`, threads cache lookups through it node-by-node, materialises bytes where they need to be, schedules work across local vs remote executors, and returns the demanded final value.

## Step 8: Where each exec actually runs — the scheduler decides

`@vixen/cargo` doesn't pick. It writes `exec` calls that fall back to `executor_class "default"`. The runtime's scheduler decides where each one actually runs, weighing:

- **What's available right now.** Is the local host capable of this work? Is the Vixen executor fleet reachable? Is the relevant executor class warm?
- **Online status.** Offline → local-only.
- **What the customer pays for.** Tier 2 customers get remote execution; Tier 1 customers don't.
- **Historical build data.** Where did this exec succeed last time? Where did it run fastest? Where did it cache-hit?
- **User-supplied constraints.** Workspace config, CLI flags, environment — the customer can pin a build to local, prefer remote, restrict to a subset of executor classes.

The scheduler is supposed to be smart. The user is supposed to be able to constrain it. The detailed shape — exact inputs, priority rules, fallback semantics, how the constraint surface looks from `@vixen/cargo`'s perspective — is genuinely open design work. See [Task: design the executor scheduler](/design-executor-scheduler/).

## Step 9: Final binary, link, and output materialisation — open

Once all units compile, the linker runs (another `exec`, with the rustup-supplied linker as args[0] and the `.rlib`s referenced as inputs). The result is a Blob (the final binary bytes) or a Tree (the binary plus sibling cargo artifacts).

`@vixen/cargo::main` returns that value. `vx`-the-CLI takes it and is responsible for landing it on the customer's filesystem at the conventional cargo location (`target/release/foo` for a release build, plus `target/release/build/...`, `target/release/deps/...`, etc.).

**How that actually happens is open.** Options under consideration:

- **Copy bytes into `target/`.** Simple, but duplicates store content on disk — every build artifact lives once in the content-addressed store and once again under `target/`.
- **Hard-link to store entries.** Doesn't work cleanly because the store uses content-defined chunking; a file's bytes live as multiple chunks in the store, no single inode to hard-link from.
- **`target/` is a Vixen VFS mount.** The customer's `target/` directory IS a virtual filesystem the runtime backs against the store. Materialising is exposing paths through the VFS, no byte copy at all. Cleanest from a "everything is content-addressed" perspective; requires the customer's local machine to host a VFS mount for the duration of the build's afterlife.

See [Task: design build-output materialisation](/materialise-build-output/).

The link step itself is structurally just another `exec` node in the graph; the *interesting* part is what happens after `main` returns, at the boundary between Vixen-evaluated values and the host filesystem.

## Open sub-tasks identified so far

- File shape for the Vixen entry: separate `vixen.styx` / `vx.build` vs frontmatter on `main.vx`. Deferred earlier; see [Packages and publishing](/packages-and-publishing/).
- Mechanism / name for `vx init` (the command that bakes current defaults into the workspace).
- The shape of the arg-parsing library Vixen provides.
- Filtering policy for env passed to `main` (all of it, a curated subset, a configurable set).
- Tree-from-host-path ingestion: the `vx` CLI has to ingest the customer's repo into the local store and produce a Tree handle before invoking `main`. That ingestion is at the host-process boundary, not inside Vixen — needs an intrinsic / runtime path. Chunking algorithm (CDC variant) is an implementation choice.
