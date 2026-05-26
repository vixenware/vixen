+++
title = "Tier 1 — CCC"

[extra]
type = "Vision"
status = "living"
created = "2026-05-26"
updated = "2026-05-26"
tags = ["strategy", "ccc", "tier-1", "cargo"]
+++

CCC is a [[CCC is a cargo fork, not a rustc wrapper|cargo fork]] distributed as a [[CCC is distributed as a rustup mirror|rustup mirror]]. It intercepts the Rust build at the cargo level — *not* just the rustc shell-out — and consults the Vixen cache before doing any work. When the cache has the relevant artifact, the fork skips entire subgraphs of compilation, linking, and `build.rs` execution. See [[Subgraph elision]] for why that is the foundational property, not the rustc-cache framing it superficially resembles.

## Shape

- **Drop-in for any cargo project.** No build system migration. No `.vx` files.
- **Runner-agnostic.** GitHub-hosted runners, self-hosted, BuildJet, namespace.so — CCC is a client-side intercept, not an execution environment. We do not provide runners.
- **One-line CI adoption.** Replace the rust-toolchain install step with a Vixen-flavoured variant that points rustup at our mirror URL. Add `permissions: id-token: write`. Nothing else.
- **No long-lived tokens.** Authentication is OIDC trusted-publishing — see [[CCC authentication uses OIDC trusted publishing]]. Cache endpoint URL is baked into the fork; zero customer config.
- **Soft-failure semantics.** Cache unavailable → CCC degrades to plain cargo. Slower, never broken.
- **Orthogonal cost line.** Runner spend and cache spend scale independently. Cache wins shrink the build window, which shrinks the runner SKU customers need. `actions/cache` for `~/.cargo` and `target/` can be removed or shrunk — optional, not required.

## Why a fork, not a wrapper

`RUSTC_WRAPPER` caches the rustc invocations it sees, materialises every intermediate `.rlib` to disk, and runs cargo's full traversal on every build. A fork sees the whole dep graph, can decide *not to invoke rustc at all* when the downstream artifact is cached, and can skip whole subgraphs. The CPU-second savings are similar; the latency savings are orders of magnitude apart.

Detail in [[CCC is a cargo fork, not a rustc wrapper]] and [[Subgraph elision]].

## Distribution

A Vixen-operated rustup mirror serving the standard rustup protocol. Customers swap their rustup source URL; the install command itself is unchanged. The mirror serves the official rustup manifests with our cargo (and eventually rustc — see [[Does cargo know enough to pre-materialise rustc's read set?]]) substituted in.

Detail in [[CCC is distributed as a rustup mirror]].

## Trust model

CCC clients are **read-only** against the cache. Pushes happen only from trusted builders Vixen operates. Unauthenticated requests get cache misses across the board — see [[Unauthenticated CCC requests miss but contribute demand signal]]. Anonymous miss signals still feed the trusted-builder work queue, so demand-driven warming works regardless of the requester's billing status.

No customer can poison the cache by construction. See [[Trust is per-cache, not global]] and [[Demand-warmed cache]].

## Cache key correctness

Correct caching is the entire product. A cache that returns wrong bytes is worse than no cache.

The minimum key:
- The fork-cargo binary hash
- The rustc binary hash
- Cargo invocation arguments + resolved lockfile + workspace shape
- Per-rustc-step: argv, env subset, content hash of every input file, target/profile
- Working directory (sandbox-relative)

If we get the key wrong, the fix is to *tighten* (mix more inputs in). Tightening is monotonic and safe: the new keyspace is fresh, old confused entries become unreachable, they age out by LRU. Detail in [[Cache key tightening is monotonic; loosening is dangerous]].

## Adoption story

1. Visit vixen.com, link your GitHub org. (One-time.)
2. In your workflow YAML: swap the rust-toolchain install step for the Vixen variant. Add `permissions: id-token: write`. Push.
3. Builds get faster as the cache warms.
4. Optionally: shrink your runner SKU, drop `actions/cache` for cargo dirs.

If CCC misbehaves, point rustup back at the official mirror — plain cargo, one line. The customer never blocks on us.

## What CCC does *not* do

- Run their builds. Customer keeps using whichever runner they like.
- Touch their build system. Cargo workspaces stay portable to plain cargo; we do *not* extend `Cargo.toml`'s schema.
- See anything beyond Rust. Other languages, multi-platform fan-out, hermetic sandboxing — those are [[Tier 2 — Vixen Build]].
