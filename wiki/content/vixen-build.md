+++
title = "Vixen Build"

[extra]
type = "Doc"
updated = "2026-05-26"
+++

A standalone product. **Vixen Build is not CCC.** Different code, different surface, different customer commitment, different value proposition. This doc covers Vixen Build (and Vixen CI as its CI-tier surface) without conflation.

## What it is

A build system. Language, runtime, sandbox, distributed exec, executor fleet, content-addressed cache with per-edge trust, package manager — all one product. Customer adopts it by running `vx cargo build` (or `vx test`, `vx <whatever>`) instead of `cargo build`. Opt-in, explicit. Customer's runner becomes an orchestration host; actual builds run on Vixen-managed executors.

The Vixen language itself stays internal at first — customers type `vx cargo build` against `@vixen/cargo`, never authoring `.vx`. When customers eventually author `.vx` files (their own CI, their own custom build steps, their own subcommands), that's the Vixen CI surface.

## What customers do

Step from CCC: workflow installs `vx` alongside the rust toolchain (same setup-rust step). Build command changes from `cargo build` to `vx cargo build`. From the customer's side, that's the visible change.

What flips behind the scenes:

- The build itself runs on Vixen executors, not the customer's CI runner. Customer runner orchestrates.
- The sandbox + VFS + cache substrate evaluates the build hermetically — caches everything, not just rustc invocations. `build.rs` outputs, linker invocations, proc-macro expansion, codegen — all cacheable.
- Multi-language workspaces work out of the box (C / C++ / protobuf / Rust in one DAG, one cache).
- Multi-platform fan-out from one invocation (linux × macOS × windows × arm/x86).

Customer doesn't install FSKit, doesn't enable projected FS on Windows, doesn't author `.vx`. The sandbox + VFS lives on Vixen's executors, not on the customer's box.

## The Vixen CI surface (T3)

When customers want to replace GitHub Actions entirely, they author `.vx` files describing their CI: jobs, dependencies, status reporting, fly-check behaviour. This is when the Vixen language becomes a public API contract — stability commitments, semantic versioning, deprecation cycles, LSP, IDE support.

Four customer-visible capabilities split underneath:

- **Fly-check on save.** The affected slice of the build/CI graph re-evaluates locally with sub-second latency.
- **PR status reporting.** GitHub statuses API hook (and equivalents).
- **Build-event dashboards.** Live view across a fan-out of executors.
- **Cross-team analytics.** Cost attribution, affected-tests selection, build-graph diffing across PRs.

Customers buy any subset.

## How a build actually happens

When customer types `vx cargo build`:

1. `vx` starts up, finds the customer's pinned packages (or runtime defaults), invokes the `cargo` subcommand's package — `@vixen/cargo::main`.
2. `@vixen/cargo` receives `(args, env, workspace: Tree)` — workspace is a content-addressed Tree, not a host path.
3. Reads `Cargo.toml` via Tree operations and the `Toml::parse[T]` intrinsic.
4. Resolves dependencies — `Cargo.toml` + crates.io registry view + resolver version → `Cargo.lock` as a build-graph node output.
5. Fetches dep sources via `acquire` (Vixen-native, content-addressed).
6. Emits an `exec` call per compile unit. The runtime evaluates demand-driven, batches cache lookups, schedules across executors, materialises only what's demanded.
7. `main` returns the final build output as a Tree; `vx` lands it on the customer's filesystem at the conventional cargo locations.

Detailed trace: [Epic: vx cargo build, end-to-end](/vx-cargo-build-trace/).

## What's different from CCC

- **Different cache.** CCC's cache is the public Cargo Cloud Cache, keyed by `(crate, version, registry-hash, ...)` because crates.io descriptors are stable. Vixen Build's cache is the language runtime's evaluation cache, keyed by content-addressed identities of typed values (including sandbox-observed read-sets at Tier 2). Both share underlying trust disciplines (tightening monotonic, per-cache trust, declared identity) but the *mechanics* differ.
- **Different surface.** CCC ships *as* cargo. Vixen Build ships *as* `vx cargo` (and eventually `vx <anything>`). Different binary, different command, different opt-in.
- **Different scope.** CCC caches what cargo invokes as rustc-shaped units, on Linux runners with stable rustup. Vixen Build caches everything, runs on every OS with its native sandbox+VFS, supports any language packaged as `@vixen/<lang>`.
- **Different trust ask.** CCC is "cargo, transparently with cache hits." Vixen Build is "a different build system that handles your build the way cargo would." A/B-verifiable; Tier 1 is the on-ramp that builds trust before Tier 2 widens the surface.

## Subsystem docs

- [Vixen language](/vixen-language/) — runtime-vs-Vixen split, FFI shape, hashable/comparable/serialisable property, `with` kwargs, effect annotations.
- [Build and resolution](/build-and-resolution/) — hermetic vs discovered inputs, obligations, lockfile-as-output, PubGrub for cargo + Vixen package resolution.
- [Packages and publishing](/packages-and-publishing/) — manifest + entry-point shape, scoped names, Vixen-org identity flow, OIDC publish, registry-as-index over Store.
- [Executor abilities](/executor-abilities/) — typed-value abilities (not identities), three-layer separation (abilities / identity / availability), probe lifecycle, verification + stale-marking.
- [Epic: vx cargo build, end-to-end](/vx-cargo-build-trace/) — the runtime trace.

## Cross-cutting docs

- [Cache and trust](/cache-and-trust/) — the disciplines that apply to *both* CCC and Vixen Build, with mechanism differences called out per product.
- [Architecture and infrastructure](/architecture-and-infrastructure/) — components and where they run.
- [Glossary](/glossary/) — short definitions.
