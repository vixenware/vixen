+++
title = "vixen"
description = "A Rust build engine with correct caching"
+++

vixen (`vx`) is a build engine for Rust with correct caching.

## Getting Started

```
cargo xtask install
```

This installs `vx` and `vxd`. On Linux, you also need `libfuse3` and `fusermount`.

Then try it on your project:

```
vx build
```

## Quick Links

- [Download](/download/) - Install vixen on macOS
- [Specification](/spec/) - Full specification document
- [Internals](/internals/) - How vixen builds things under the hood
- [Daemon Topologies](/daemons/) - Local vs distributed daemon lifecycle model
- [RulePlan Schema](/rule-plan/) - Typed planning IR and validation rules
- [Planner Runtime Boundary](/planner-runtime/) - In-process host/interpreter contract
- [Planner Builtins and Capability Policy](/planner-builtins/) - Deterministic builtin catalog and guardrails
- [RulePlan Lowering Bridge](/rule-plan-lowering/) - Starter RulePlan -> ActionGraph mapping
- [GitHub](https://github.com/bearcove/vixen)

## Overview

vixen builds single-crate Rust projects using content-addressed storage and incremental computation. If inputs haven't changed, the second build is instant — zero compiler invocations.

```
vx build           # debug build
vx build --release # release build
vx clean           # remove .vx/ directory
```

## Architecture

vixen uses a daemon-based multi-process architecture:

- **vx** — Thin CLI client. Connects to the daemon via roam RPC over a Unix socket.
- **vxd** — The daemon. Orchestrates builds using picante incremental queries. Parses manifests, constructs action graphs, executes actions.
- **vx-runner** — Remote compilation worker. Executes compile and link actions in isolated environments using CAS.
- **vx-store** — Content-addressable storage. Stores toolchains, sources, and build artifacts as blake3-hashed blobs.

## Key Properties

- **Correct caching** — Cache keys are computed from the full set of inputs (source content, compiler flags, toolchain version). Changed inputs always trigger a rebuild; unchanged inputs never do.
- **Hermetic toolchains** — Rust and Zig toolchains are downloaded and stored in the global CAS. No system dependencies beyond a linker.
- **Content-addressed storage** — All artifacts are identified by their blake3 hash. Enables deduplication and integrity verification.
- **Incremental computation** — Uses picante for dependency tracking and single-flight memoization. Only affected queries re-run on changes.

## Multi-Language Support

vixen supports C/C++ compilation via hermetic Zig toolchains, configured through `vx.styx` project manifests:

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

## FAQ

### Why so many processes?

vixen is designed for remote execution. Each component can run on a different machine:

- `vx-store` could be in the cloud — a shared cache for your whole team
- `vx-runner` could be on a beefy VM somewhere, doing the actual compilation
- `vxd` is the local daemon that orchestrates everything

When you develop locally, all of these just happen to run on the same machine. But the separation means that when you run `vx install somecrate` and someone else on your team has already built it, vixen just downloads the result from the shared store instead of compiling it again.

## Current Scope (v0)

vixen deliberately limits scope in v0 to get the foundations right:

- Single-crate Rust projects (no workspaces)
- No external dependencies, features, build scripts, or proc macros
- No tests, benches, or examples
- Single binary target
- C/C++ via vx.styx manifests (pure C MVP)
