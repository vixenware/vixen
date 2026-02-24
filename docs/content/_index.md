+++
title = "vixen"
description = "Experimental Rust build engine focused on hermetic builds and correct caching"
+++

## Why? {#why}

There's an unexplored middle ground between the "spell it all out" Bazel/Buck2
universe and Cargo's "best-effort caching". vixen aims for hermetic execution
without making day-to-day Rust work feel like operating heavy machinery.

To this end, vixen:

  * Treats orchestration, storage, and action execution as separate concerns
    that do not have to run on the same node.
  * Enforces a virtual filesystem and sandboxing layer on all supported
    platforms (macOS, Linux, Windows).

Those constraints are exactly why vixen is not a Cargo successor. Cargo must
work everywhere, for everyone, with minimal prerequisites.

Compared to other hermetic build systems, vixen is deliberately Rust-first:

  * A rule language designed to accommodate Rust's build-system shape without
    requiring pre-build file generation.
  * Support for a "Cargo.toml only" mode of operation.
  * A planned "fixup package manager" for crates with special needs
    (creative build scripts or proc macros), only if that becomes necessary.

## Current status

vixen is in early development. Today it builds a narrow Rust subset: single-crate,
single-binary projects with no dependencies, no features, no build scripts
(`build.rs`), no proc macros, and no tests/benches/examples/workspaces. If your
project falls outside that box, vixen should fail clearly rather than attempt a
partial build.

Important foundations are already in place (CAS, VFS, sandboxing), and the rest
is being built incrementally. If you want to follow progress, join
[the Discord](https://discord.gg/GJadP5ggXs).

## Getting started

- [Prerequisites](/prerequisites/) - Platform requirements for Linux, macOS, and Windows
- [Download](/download/) - Install the macOS app and CLI entrypoints
- [GitHub](https://github.com/bearcove/vixen)

## Documentation

- [Specification](/spec/) - Current guarantees, scope, and specification roadmap
- [Internals](/internals/) - How vixen works under the hood
- [Daemon topologies](/daemons/) - Local vs distributed daemon lifecycle model
- [Vixen mac app](/vixen-mac-app/) - Why the app exists and how FSKit integration works
