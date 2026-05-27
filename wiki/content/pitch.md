+++
title = "The pitch"

[extra]
type = "Doc"
tiers = ["1", "2", "3"]
updated = "2026-05-26"
+++

Vixen ships as three tiers.

| Tier | Customer command | `.vx` authoring |
| ---- | ---------------- | --------------- |
| 1 — CCC | `cargo build` | No |
| 2 — Vixen Build | `vx cargo build` (opt-in) | No |
| 3 — Vixen CI | `vx …` against customer-authored `.vx` | Yes |

## Tier 1: CCC

A cargo fork. The fork has cargo's whole dep graph and the stable identity of every unit it would otherwise build — for crates.io deps that's `(crate-name, version, registry-hash)`, period. Version immutability on crates.io makes that descriptor *the* identity; no source file hashing involved.

**Mechanism on a build:**

1. Fork resolves the dep graph, the same as cargo would.
2. Fork sends the entire set of unit identities to ccc-backend in **one RPC**.
3. ccc-backend returns the hit/miss partition: "I have these, I don't have these."
4. For the leaves of the cache-hit set the user actually demanded (typically just the final binary), fork downloads the artifact and writes it to `target/`. Intermediate `.rlib`s the user doesn't actually need are never fetched.
5. For cache misses, fork invokes rustc locally as usual. ccc-backend already knows about those misses — the graph lookup in step 2 was the demand signal; ccc-backend queues backfill on the trusted-builder fleet as part of answering that RPC.

**One round-trip carries the whole graph status.** Sequential per-unit lookups would burn N round-trips on a fully-cold build, asking for ghosts.

**Scope at launch:**

- Linux only (sandboxing is landlock-only today).
- Stable rustup toolchains only (no nightly, no custom).
- GitHub Actions only (the OIDC verifier ships per-provider; GitLab / Buildkite / CircleCI come later).

**Workflow diff:**

Before (existing typical CI):

```yaml
- uses: dtolnay/rust-toolchain@stable
- uses: actions/cache@v3
  with:
    path: |
      ~/.cargo
      target
    key: ${{ runner.os }}-cargo-${{ hashFiles('Cargo.lock') }}
- run: cargo build --release
```

After:

```yaml
permissions:
  id-token: write
steps:
  - uses: vixen/setup-rust@v1            # was dtolnay/rust-toolchain
  - run: cargo build --release           # unchanged
```

One line swapped, one permission added, `actions/cache` removed. No env var, no token to manage.

**Distribution:** the Vixen-flavoured cargo is shipped via a Vixen-operated rustup mirror. The customer's setup step points rustup at our URL. Install command is otherwise unchanged.

**Auth:** OIDC trusted publishing. No long-lived tokens. Cache endpoint baked into the fork.

**Read-side trust:** unauthenticated requests miss across the board. No free tier today. Anonymous misses still feed the trusted-builder queue, so the cache warms regardless of paying status.

**What CCC at Tier 1 does *not* do:**

- Cache anything other than cargo-known compile units. Linker invocations, `build.rs` execution, arbitrary toolchain bytes — all run as normal. (That's Tier 2.)
- Redirect crates.io fetches. The trusted-builder fleet fetches from crates.io itself during backfill. Customer cargo goes direct.
- Hash source files. Identity is the crates.io descriptor.
- Support private crates. No private cache at Tier 1. Workspace-local crates (anything not on crates.io) just rebuild locally on miss.

## Tier 2: Vixen Build

Customer types `vx cargo build` instead of `cargo build`. Opt-in, not transparent. We don't switch things under the customer's feet.

`vx` is installed by the same setup-rust action that bootstraps CCC; the customer changes one line in their workflow to invoke `vx cargo build` and is now on Tier 2.

The build runs on Vixen executors. Sandbox + VFS + cache live on our side. Customer's runner orchestrates; doesn't compile.

Beyond Tier 1: caches everything the build invokes — `build.rs`, linker, proc-macros, codegen. Multi-platform fan-out. Cross-language. Provenance attestation.

Customer doesn't author `.vx` files. The `vx cargo` subcommand is implemented by the `@vixen/cargo` package we ship; customer consumes it as a black box.

Vixen language stays internal at this tier. Customers depend on the UX of `vx cargo build`, not on the language they don't write. We can rev language internals while the subcommand's observable behaviour stays stable.

Trust ask: customer is now on Vixen-controlled execution. Verifies compatibility by A/B against plain cargo.

## Tier 3: Vixen CI

Customer writes `.vx` files. `vx` CLI ships.

Subcommands come from packages, not from the runtime. `vx cargo build` invokes `@vixen/cargo`. `vx python …`, `vx protobuf …` once those packages exist.

CI lives in the repo as `.vx`, not in `.github/workflows/*.yaml`. GitHub Actions becomes unnecessary.

Tier 3 splits into four capabilities, priced separately or bundled:

- Fly-check on save — velocity, every org size.
- PR status reporting — anyone using PRs.
- Build-event dashboards — fan-out builds.
- Cross-team analytics — multi-team orgs.

## Sequencing

Tier 1 funds Tier 2's R&D. Tier 2 ships without finishing the Vixen language. Tier 3 unlocks once the language is stable enough to expose. Tier 2 work doesn't wait for Tier 1.

## Buyers

- Tier 1: engineer with slow Rust CI.
- Tier 2: platform team wanting speed + hermeticity + multi-language + provenance, without owning a new build system.
- Tier 3: small org — devs paying for fly-check; large org — compliance/SRE/platform-engineering.

## Pricing

Value-aligned meters, not cost-aligned. We pay for bandwidth and compute; customers don't see those line items. Two axes: subscription tier (capability) + usage meter (volume).

Tier 2 ladder (proposed):

| Tier            | Public cache       | Private cache | Dashboards | Audit / on-prem |
| --------------- | ------------------ | ------------- | ---------- | --------------- |
| Free (deferred) | anon, low priority | —             | —          | —               |
| Starter         | quota'd            | —             | —          | —               |
| Team            | larger quota       | yes, sized    | basic      | —               |
| Enterprise      | unlimited          | unlimited     | full       | yes             |

Meters: hit count (each hit = a build we saved), demand-priority fee per authenticated miss, private-cache storage per GB-month.

Largest perceived jump is Free → Starter. Starter → Team enables proprietary code, not raw speed.

## What Tier 2 unlocks

Things requiring "what bytes produced what outputs, signed by whom, deterministically":

1. Cross-language hermetic builds
2. Remote execution fleet by executor class
3. Multi-platform fan-out from one invocation
4. SLSA-grade provenance attestation
5. License/origin policy at build time
6. Self-hosted executor pools (on-prem, air-gapped)
7. Programmable lockfile policy (no CVEs, internal-mirror-only)
8. Build observability with structured traces
9. Dev-laptop cache sharing (per-seat)
10. Toolchain attestation
11. Affected-only test execution
12. Reproducibility quorum (N executors must agree bit-for-bit)
13. Time-travel build replay
14. Bisect-as-a-service against the cache
15. Sub-file incremental invalidation via sandbox read-set
16. Build-graph diffing across commits
17. Speculative pre-warming
18. Closure-shipped one-off compute (ML, data-eng)
19. Sandbox-as-product, standalone
20. Turing-complete dep policy in the build language
21. Per-team cost attribution
22. Per-input cache hit-rate analytics
23. JIT'd Vixen kernels
24. Cross-org cache federation
25. TTL/compliance caches with mandated re-attestation
26. Build-event streams to live dashboards

Tier 1 is "fast cargo." Tier 2 is "we own the build epistemology."
