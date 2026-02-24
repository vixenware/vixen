+++
title = "vixen"
description = "Experimental Rust build engine focused on hermetic builds and correct caching"
+++

## Why vixen? {#why}

vixen explores the space between fully explicit hermetic build systems
(Bazel/Buck2-style) and Cargo's default developer experience.

The current design goals are:

- Keep build execution hermetic and cache-safe
- Keep setup and day-to-day workflow lightweight
- Separate orchestration, execution, and storage so they can run on one host or across multiple hosts

vixen is not a Cargo replacement. Cargo needs to work in many environments where
vixen's virtual filesystem and sandbox requirements are not available.

## Current status

vixen is in early development. Today it supports a narrow Rust subset:

- Single-crate projects
- Single binary target
- No dependencies
- No features
- No build scripts (`build.rs`)
- No proc macros
- No tests, benches, or examples
- No workspaces

If your project falls outside these constraints, expect a clear error rather than
partial behavior.

## Getting started

- [Prerequisites](/prerequisites/) - Platform requirements for Linux, macOS, and Windows
- [Download](/download/) - Install the macOS app and CLI entrypoints
- [GitHub](https://github.com/bearcove/vixen)

## Documentation

- [Specification](/spec/) - Current guarantees, scope, and planned specification work
- [Internals](/internals/) - Storage, runner, diagnostics, and rules direction
- [Daemon topologies](/daemons/) - How `vx` and daemon processes are started and managed
- [Vixen mac app](/vixen-mac-app/) - Why the app exists and how FSKit integration works
