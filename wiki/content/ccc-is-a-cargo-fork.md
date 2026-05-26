+++
title = "CCC is a cargo fork, not a rustc wrapper"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["ccc", "tier-1", "architecture"]
+++

## Context

CCC needs to intercept Rust build invocations to consult and populate the Vixen cache. There are several lower-touch options that don't require forking cargo: `RUSTC_WRAPPER`, a sccache-shaped sidecar, a rustup component. Each is structurally cheaper to maintain than a cargo fork.

The question is whether the cheaper options are sufficient.

## Options considered

1. **`RUSTC_WRAPPER` environment variable.** Cargo natively invokes `$RUSTC_WRAPPER $rustc $args` instead of `$rustc $args`. Zero patching of cargo; the wrapper is a single binary set via env var. This is what sccache, cachepot, ccache-rs use.
2. **A sccache-shaped sidecar.** Same `RUSTC_WRAPPER` mechanism, particular wire-protocol shape.
3. **A cargo fork.** Modify cargo itself.

## Decision

A cargo fork. `RUSTC_WRAPPER` and sidecars are insufficient.

## Consequences

- **Tighter cache keys.** A wrapper only sees the rustc invocation. It doesn't see the workspace, the lockfile, the resolved dep graph, the target or profile, whether this is `build.rs` vs lib vs bin. Argv parsing recovers some of that, unreliably. A fork sees all of it natively.
- **Coverage beyond rustc.** A wrapper wraps only rustc. Linking, `build.rs` execution, proc-macro expansion, codegen-backend invocation — none of those have cargo-level wrapper hooks. To cache them, you have to be inside cargo.
- **Crates.io fetches.** A fork can redirect crates.io fetches to a Vixen-controlled artifact source. A wrapper cannot.
- **Subgraph elision.** A wrapper caches the invocations it sees; a fork can decide *not to invoke rustc at all* when the downstream artifact is already in cache. See [[Subgraph elision]] — this is the foundational property that separates CCC from sccache-shaped solutions.
- **Maintenance cost.** Tracking upstream cargo releases. This is intentionally absorbed; CCC's distribution channel ([[CCC is distributed as a rustup mirror]]) is designed around it.
- **We do *not* extend `Cargo.toml`'s schema.** A fork *could* add config sections; we deliberately don't. Source repos stay portable to plain cargo.

## Related

- [[Tier 1 — CCC]]
- [[CCC is distributed as a rustup mirror]]
- [[Subgraph elision]]
- [[Does cargo know enough to pre-materialise rustc's read set?]]
