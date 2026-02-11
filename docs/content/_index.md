+++
title = "vixen"
description = "A Rust build engine with correct caching"
+++

## Why? {#why}

vixen computes cache keys from the actual content of inputs — source files,
compiler flags, toolchain version — and never invokes the compiler unless
something really changed.

## Architecture

Builds are operated by a fleet of daemons, which allows caching and executing
actions remotely. Even local builds are sandboxed.

- **vx** — Thin CLI front-end
- **vxd** — Orchestrates builds, dispatches actions
- **vx-runner** — Executes compile and link actions in isolated environments
- **vx-store** — Content-addressed storage for toolchains, sources, and artifacts

## Current status

vixen is in early development. It can build single-crate Rust projects with no
dependencies, no proc macros, no build scripts, and a single binary target.

## Documentation

- [Internals](/internals/) — How vixen builds things under the hood
- [Daemon Topologies](/daemons/) — Local vs distributed daemon lifecycle model
