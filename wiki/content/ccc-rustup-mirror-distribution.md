+++
title = "CCC is distributed as a rustup mirror"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["ccc", "tier-1", "distribution"]
+++

## Context

CCC is [[CCC is a cargo fork, not a rustc wrapper|a cargo fork]]. It needs to land on every developer machine and every CI runner the customer uses. The distribution mechanism shapes both adoption cost and how we track upstream cargo releases.

The Rust project is not going to ship our private fork through official channels — we have to host distribution ourselves.

## Options considered

1. **Ship a standalone binary** — "download `vxcargo` and put it on `PATH`." Hostile to adoption; introduces a new tool name; doesn't get rustup's toolchain coordination for free.
2. **Negotiate a separate rustup channel** with the Rust project (a `vixen` channel alongside `stable`/`beta`/`nightly`). Won't happen — the Rust project will not distribute a private fork.
3. **Run a rustup mirror.** Standard rustup protocol; we host the manifests and binaries; customers point rustup at our mirror URL instead of `https://static.rust-lang.org`.

## Decision

A rustup mirror. Standard rustup protocol, Vixen-controlled source URL.

## Consequences

- **Install command unchanged.** `rustup install stable` works as before; only the source URL flips. Customers don't learn a new tool.
- **Coherent toolchain versioning.** "stable 1.91 (vixen mirror)" tracks "stable 1.91" upstream. We substitute the cargo binary inside the toolchain manifest, leave rustc/clippy/rustfmt as upstream (until [[Does cargo know enough to pre-materialise rustc's read set?]] forces a rustc fork too).
- **One-line CI adoption.** Replace the `dtolnay/rust-toolchain` (or `actions-rust-lang/setup-rust-toolchain`) step with a Vixen-flavoured variant that points rustup at our mirror URL. No second action, no separate install step.
- **`actions/cache` becomes optional.** With CCC handling the cache, the customer can disable or shrink their existing `actions/cache` for `~/.cargo` and `target/`. Optional, not required — soft optimisation, not a regression if left in place.
- **We host distribution infrastructure.** A rustup mirror needs availability, geographic distribution, signing keys, manifest publication. This is a permanent operational cost. We absorb it because nothing else gets us the one-line adoption story.
- **Mirror upstream + substitute.** Our mirror serves the official rustup manifests with a small set of components swapped for Vixen-built equivalents. Customers who don't want CCC can point rustup back at the official mirror; we don't fork the world.

## Related

- [[Tier 1 — CCC]]
- [[CCC is a cargo fork, not a rustc wrapper]]
- [[CCC authentication uses OIDC trusted publishing]]
