+++
title = "Specification"
insert_anchor_links = "heading"
+++

# Vixen Specification

This document describes the design goals and intended behavior of vixen, a
build engine for Rust (and more). It is a draft — vixen is early and most of
what follows is aspirational. Where something is implemented, we say so.
Where it isn't, we say that too.

## Conventions

This spec uses **MUST** for normative requirements. Normative requirements
appear in blockquotes with a requirement identifier (e.g. `r[...]`). All
other text is informative. If you can't write a test for it, it's not a
requirement.

## What Vixen Is

vixen (`vx`) is a build engine. It compiles source code into executables and
libraries. It is not a package manager (yet). The CLI is `vx`.

```
vx build           # debug build
vx build --release # release build
vx clean           # remove .vx/ directory
```

## Design Axioms

These are the non-negotiable properties that define vixen. Everything else
is a feature that may or may not exist yet.

### Hermetic Toolchains

> r[axiom.hermetic]
> vixen MUST acquire and manage its own toolchains. No system-installed
> compiler or linker may appear in the build path. All tools used during
> a build MUST be obtained by vixen and stored in content-addressed storage.

vixen downloads Rust toolchains from static.rust-lang.org and Zig toolchains
for C/C++ compilation and linking (including lld). The system provides
nothing beyond a kernel and a way to run executables.

This is partially implemented. Rust toolchain acquisition works. Zig
toolchain acquisition works. Linking currently uses the Zig-bundled lld for
C targets. Rust targets still use the system linker in some configurations.

### Content-Addressed Storage

> r[axiom.cas]
> All build artifacts, toolchains, and source snapshots MUST be stored in
> content-addressed storage, identified by their blake3 hash.

This means:
- Two identical artifacts are stored once (deduplication).
- An artifact's identity is its content, not its path or timestamp.
- Integrity is verifiable: re-hash and compare.

Implemented. The `cass` service handles blob storage and manifest tracking.

### Correct Caching

> r[axiom.cache-correct]
> If the cache says an artifact is valid, it MUST be valid. A cache hit MUST
> produce the same output as a fresh build with the same inputs. A change to
> any input MUST invalidate the cache.

No false positives (stale cache served as fresh). No false negatives are
acceptable as a trade-off (a cache miss when a hit was possible is wasteful
but not incorrect).

Cache keys are computed from the full set of inputs that affect build output.
The exact mechanism for determining "the full set of inputs" is still in flux
— the current approach uses rustc dep-info output from previous builds, but
this is a first shot, not a final design.

### Deterministic Cache Keys

> r[axiom.cache-deterministic]
> Cache key computation MUST be deterministic. The same inputs MUST always
> produce the same cache key, regardless of the order in which they are
> provided.

This means collections (dependencies, compiler flags, include paths, defines)
are sorted before hashing. Cache key format versions are bumped when the
computation changes.

Implemented for both Rust and C targets.

### Distributed Builds

> r[axiom.distributed]
> vixen's architecture MUST support distributed builds. Compilation MUST be
> separable from orchestration. Build artifacts MUST be transferable between
> machines via CAS.

vixen uses a daemon architecture with three roles:
- **aether** — orchestration. Parses manifests, builds action graphs,
  schedules work.
- **rhea** — execution. Runs compiler invocations in isolated environments.
- **cass** — storage. Stores and retrieves content-addressed blobs and
  manifests.

These communicate via roam RPC. Today they run as threads in the same process.
The architecture is designed so that rhea and cass can run on separate machines.

### Shared Cache

> r[axiom.shared-cache]
> CAS MUST be shareable across machines. Two builds with identical inputs on
> different machines MUST be able to share cached artifacts through a common
> CAS instance.

This is the big payoff of hermetic toolchains + content-addressed storage.
If your toolchain is the same (same hash) and your sources are the same
(same hash), the build output is the same (same hash). A shared CAS means
build results from one machine benefit every other machine.

Not yet implemented. CAS is currently local (`~/.vx/`). Shared/remote CAS
is a first-class goal.

### Multi-Language

> r[axiom.multi-lang]
> vixen MUST support building projects that combine multiple languages in a
> single build graph. Rust and C/C++ are the initial targets. The architecture
> MUST be extensible to other languages (TypeScript, CSS, etc.) without
> redesigning the core.

Cargo builds Rust. vixen builds programs. Real programs are not single-language.
The self-hosting goal (vixen building itself) requires C++ support, since Rust
depends on LLVM.

C compilation via Zig is implemented (pure C MVP). C++ is next. The action
graph is language-agnostic: it operates on compile and link actions, not on
"Rust compiles" specifically.

### Inspectable Build Graph

> r[axiom.inspectable]
> The build action graph MUST be inspectable. Users MUST be able to see what
> actions will run, what their inputs are, and whether each action is a cache
> hit or miss.

`vx build --explain` exists and shows the action graph. This is a first-class
feature, not a debugging tool.

Partially implemented.

## Architecture

### Daemon Model

> r[arch.daemon]
> vixen MUST use a daemon architecture. The CLI (`vx`) is a thin client that
> communicates with the daemon (`aether`) via RPC over a Unix socket.

The daemon stays alive between builds, preserving the incremental computation
state. This means the second `vx build` in a session is faster than the first
even without cache hits, because query results are memoized in memory.

Implemented.

### Incremental Computation

> r[arch.incremental]
> vixen MUST use incremental computation for build orchestration. Only
> queries affected by changed inputs MUST re-execute.

vixen uses picante, an async incremental query runtime. Queries declare
dependencies via task-locals and are automatically re-executed when their
inputs change. Results are memoized with single-flight deduplication.

Implemented.

### Action Graph

> r[arch.action-graph]
> vixen MUST construct a directed acyclic graph of build actions. Actions
> MUST declare their inputs and outputs. The graph MUST be constructed before
> execution begins.

Action types include:
- Toolchain acquisition (Rust, Zig)
- Source ingestion into CAS
- Compilation (Rust, C, C++)
- Linking

The graph is built by aether and executed in dependency order. Independent
actions can execute in parallel.

Implemented.

### Fingerprinting

> r[arch.fingerprint]
> Anything that affects build output MUST be included in the cache key.
> If vixen cannot fingerprint a tool or input, it MUST NOT use that tool
> or input.

This is the corollary of hermetic toolchains. System-provided tools are
excluded from the build path not out of principle but because they can't
be reliably fingerprinted. If you can't hash it, you can't cache it.

### Source Snapshots

> r[arch.snapshot]
> vixen MUST snapshot source files into CAS before building. The snapshot
> provides a stable, content-addressed view of the source tree that can be
> transferred to a remote worker.

Snapshots use tree manifests: sorted lists of (path, blob_hash) entries.
The snapshot is intentionally maximalist (includes all files that might be
relevant) for safe transport. Cache keys are computed from the subset of
files actually observed by the compiler, not from the snapshot.

Implemented.

## Project Configuration

### vx.styx

> r[config.vx-styx]
> vixen's native project manifest format MUST be `vx.styx`, written in the
> Styx configuration language.

```styx
project {
    name hello
    lang c
}
bins ({
    name hello
    sources main.c
})
```

Implemented for C projects. Rust projects currently fall back to Cargo.toml.

### Cargo.toml Compatibility

> r[config.cargo-compat]
> vixen MUST be able to build Rust projects from an existing Cargo.toml
> without requiring a vx.styx file.

vixen parses Cargo.toml directly. It does not shell out to cargo. The parser
understands package metadata, targets, path dependencies, and registry
dependencies.

Implemented (with limitations — many Cargo.toml features are not yet
supported).

## Current Status

vixen is at v0. The axioms above are the design. Here is what actually works
today:

- Single-crate Rust projects with no dependencies
- Single-crate Rust projects with path dependencies
- Pure C projects via vx.styx + Zig
- Hermetic Rust toolchain acquisition
- Hermetic Zig toolchain acquisition
- Content-addressed storage (local)
- Deterministic cache keys
- Incremental computation via picante
- Build action graph with parallel execution
- CLI with build, clean, and explain commands

Here is what does not work yet:

- Workspaces
- Registry dependencies (crates.io)
- Features
- Build scripts (build.rs)
- Proc macros
- Tests, benches, examples
- C++ compilation
- Remote/shared CAS
- Remote execution (rhea on a separate machine)
- Bit-for-bit reproducibility verification
- Sub-crate incremental compilation
