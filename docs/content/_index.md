+++
title = "vixen"
description = "Experimental build system focused on hermetic builds and correct caching"
+++

## Why? {#why}

Vixen was born out of frustration with the existing solutions when it comes to
building Rust code, which today mostly amount to `cargo build` and
[Buck2](https://buck2.build/).

The former is not built for hermeticity, remote execution, multi-language
builds, precise caching, etc. The latter is, but hot damn that's a lot of BUCK
files to write (or generate, ahead of time, with reindeer etc.)

Originally designed as a "large Rust core" with a "small build language", Vixen
has now progressed to the "small Rust core" with a "fully-featured build language"
stage.

Due to this major design change, what little used to work in Vixen currently doesn't.

Because the language and the semantics are still in flux, there exists basically
no public documentation about the language itself. It is, however, possible to
list the design goals.

## What {#what}

Vixen is a demand-driven language: any expression that is not demanded by the
final build result is not evaluated. This means `a + b` doesn't "force
evaluation" of `a`, and neither does `[a, b]`, neither does `some_func a`,
or even `a.field`.

Vixen has value semantics, there are no mutable references (or immutable references,
for that matter). It is strongly typed, has Rust-like structs, tuples and enums.

Vixen is designed for distributed evaluation: closures can be passed over the network
to be evaluated on other hosts.

Vixen is designed to solve problems like version resolution (think Cargo.toml =>
Cargo.lock), not by describing the algorithm to use to solve version resolution,
but by describing the shape of the problem through usage of regular function
application, annotations like `key`, `join`, and others (still under heavy research).

Vixen is designed to encourage fully hermetic setups, but takes a pragmatic
approach to sandboxing and ambient toolchains via "fact discovery", "attached
obligations", etc. (example: it's fine to use your local XCode as long as you've
fingerprinted it).

Vixen is designed to provide a great code authoring experience, with first-class
LSP support, and to keep a small standard library (essentially signature for
intrinsics) with a package manager from day one (distributing vixen libraries
for... finding C toolchains, etc. etc.)

The Vixen language is designed so that concurrency naturally falls out of
regular, naively-written code — static analysis is used to divide the code into
nodes of a graph: some are dependencies on an external task, some are built-ins
(like `acquire`, etc.), and some are kernels of pure vixen code, that are
meant to be JIT-compiled into native code.

It is a design goal of the Vixen language to be able to re-implement the
domain-specific parts of cargo package resolution in Vixen itself (API
endpoints, structs/enums into which JSON & YAML payloads are deserialized,
etc.), while leaving the actual graph traversal problem to the Vixen runtime. 

It is not a design goal of the Vixen language to avoid turing-completeness.

## Stay tuned

If you want to follow progress, join [the Discord](https://discord.gg/GJadP5ggXs).
