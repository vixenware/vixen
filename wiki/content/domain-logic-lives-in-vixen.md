+++
title = "Domain logic lives in Vixen, not in the runtime"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["language", "runtime", "architecture", "tier-2", "tier-3"]
+++

## Context

A build system that knows how to build Rust needs *somewhere* the Rust-specific logic lives: how to parse `Cargo.toml`, what features mean, how to resolve dependencies under cargo's rules, how to find rustc, how to invoke it for each crate, how to handle proc-macros vs lib vs bin vs build.rs. The same is true for C / C++ (find clang or MSVC, run pkg-config, invoke the linker), for protobuf (find protoc, run codegen), and for every other language we'll touch.

Two places that logic could live: in the Rust runtime (compiled-in, fast, single-source), or in Vixen code distributed as packages (interpreted, cacheable, evolvable independently).

## Options considered

1. **Domain logic in the Rust runtime.** Cargo support, C/C++ support, protobuf support — all native code. Fast. Single-source. Hard to evolve without releasing a new runtime binary; every change requires a coordinated runtime release; customers locked to one set of semantics until they upgrade the runtime.
2. **Domain logic in Vixen packages.** The runtime knows nothing about Rust or C; the package `vixen-rust` (or whatever) knows. Cacheable for free (every Vixen expression's result is cacheable). Strongly typed. Demand-driven. Versionable independently. Distributable to executors via closure shipping. Slower at the inner-loop level but trivially evolvable.

## Decision

Option 2, from day one. The Rust runtime is a generic engine: it knows how to evaluate Vixen, manage a sandbox, mount a VFS, talk HTTP, run PubGrub-style constraint solving, parse-into-typed-Vixen-values. It knows nothing about cargo, about Rust crates, about Xcode, about protobuf. All of that lives in Vixen packages.

The runtime stabilises slowly; domain packages churn fast.

## Consequences

- **Each language is a Vixen package.** `vixen-rust` knows how to talk to crates.io, fingerprint a rustc toolchain, evaluate a Cargo.toml into a build graph. `vixen-c` knows clang / MSVC. `vixen-protobuf` knows protoc. Adding a language is adding a package, not changing the runtime.
- **Cargo's resolution model is itself Vixen code from day one.** Not "eventually rewritten in Vixen" — written in Vixen from the start. The native runtime provides PubGrub as a generic constraint solver; `vixen-rust` provides cargo's semantic model (workspaces, features, `[patch]`, `[dependencies]` vs `[build-dependencies]`, etc.) on top.
- **Caching of domain logic is free.** Evaluating "what's the dep graph of this workspace" is a Vixen expression. Same inputs → same dep graph → cache hit. No bespoke cargo-resolution-cache; the language's caching covers it.
- **Versioning matters early.** Customers will lock specific versions of `vixen-rust`. Upgrading `vixen-rust` is a deliberate user action with a downgrade path. See [[Package management for Vixen]].
- **Performance is a runtime concern, not a domain concern.** If `vixen-rust`'s resolver is slow on huge workspaces, the answer is bytecode / JIT / better intrinsics — not "rewrite vixen-rust in Rust." See [[Bytecode and JIT exploration]].
- **The runtime API surface is small and stable.** Whatever intrinsics the runtime provides are the FFI; everything else is Vixen. See [[Rust/Vixen FFI shape]].

## Related

- [[Tier 2 — Vixen Build]] — substrate that ships this architecture commercially.
- [[Vixen language stays internal until Tier 3]] — the customer-facing language surface stays sealed until Tier 3, but the package mechanism is alive at Tier 2 because Vixen builds need it internally.
- [[Rust/Vixen FFI shape]] — how Vixen calls into the runtime.
- [[Package management for Vixen]] — how `vixen-rust` etc. get distributed and pinned.
- [[Bytecode and JIT exploration]] — performance path for hot Vixen code.
