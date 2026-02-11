+++
title = "vixen"
description = "The best build system that doesn't exist yet"
+++

## Why? {#why}

There's an unexplored middleground between the "spell it all out" Bazel/Buck2
universe, and the "caching is best-effort" of cargo. Vixen aims to reconcile
the properties of hermetic build systems with the user experience of cargo.

To this end, vixen:

  * Treats orchestration, storage, action execution as separate concerns
    that do not necessarily occur on the same node.
  * Enforces use of a virtual filesystem, and a sandboxing mechanism, on all
    three supported platforms (macOS, Linux, Windows)

This set of constraints alone makes it impossible for vixen to be a _successor_
to cargo. cargo must work everywhere, for everyone, on every platform, without
any additional requirements.

Compared to other hermetic build system, vixen aims to have better support for
Rust through:

  * A rule language designed to accomodate Rust's peculiar build system needs
    (without requiring pre-build file generation)
  * Support for a "Cargo.toml only" mode of operation
  * A (planned) "fixup package manager" for crates with special needs (creative
    build scripts or proc macros) — but only if we can't figure out a way for
    vixen to "just magically support everything". Wouldn't that be neat.
  
The rule language is in flux, that's why you don't see it documented here.

## Current status

vixen is in early development. It can build single-crate Rust projects with no
dependencies, no proc macros, no build scripts, and a single binary target.

It will certainly not build your project at this stage.

However, important foundations are here (CAS, vfs, sandboxing) and the rest will
fall into place over time. You're welcome to hang out in [the discord](https://discord.gg/GJadP5ggXs)
if you want to chat about the progress.

## Documentation

- [Internals](/internals/) — How vixen builds things under the hood
- [Daemon Topologies](/daemons/) — Local vs distributed daemon lifecycle model
