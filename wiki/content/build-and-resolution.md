+++
title = "Build and resolution"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-05-26"
+++

How a build actually happens: how inputs get identified, how toolchains get chosen, how the cache key is derived, and how the build commits or poisons.

## Two kinds of inputs: hermetic and discovered

A Vixen build's inputs split into two categories that coexist freely:

- **Hermetic inputs.** Fetched by Vixen from a content-addressed declaration. crates.io entries (`(name, version, registry-hash)`), public git URLs at a commit hash, `acquire(url) with { digest }` blobs. Identity is the declaration; we never have to hash the bytes ourselves.
- **Discovered inputs.** Found on the local executor's host via a probe. The user's locally-installed Xcode, MSVC, a non-Vixen-managed clang, anything the runtime did not fetch.

Mixed in the same build is fine. Hermetic for the toolchain we ship, discovered for the SDK the user already has, all under one cache key.

## Discovery produces *obligations*, not fingerprints

A discovery probe returns a `CToolchain { clang, fingerprints: [...] }`-shaped value. The `fingerprints` are constraints the discovered tool must continue to satisfy on the next build for the result to be reusable. Together they form an **obligation**: a claim to verify later.

Examples from `zoo/c.vx`:
- `host.exec([clang, "--version"])` — the output bytes of `clang --version`.
- `host.exec(["xcode-select", "--print-path"])` — the active developer dir.
- `host.exec(["plutil", "-p", "/Applications/Xcode.app/Contents/Info.plist"])` — version metadata.

The obligation is the tuple of those observed bytes. If on the next build the re-observed bytes match, the obligation holds. If they don't, the obligation is violated and the build derived from it is invalid.

## Cache key can be computed upfront, with a catch

Once resolution has picked concrete toolchains, the cache key is concrete. For each build invocation:

- Hermetic inputs contribute their content-addressed identity.
- Discovered inputs contribute their obligation tuples.

The fork derives the cache key from these and consults the cache before running anything.

The catch: when *reusing previous obligations* — assuming the host is the same as last build because the cheap check passed — the cache key is a bet that those obligations still hold. The actual verification (re-observing the bytes, hashing the discovered files) runs *concurrently with the build*. If verification finishes before the artifact commits and disagrees with the assumption, the build is **poisoned** — thrown away, re-dispatched with corrected obligations under the actual cache key for *those* values.

Verification races with the work. Whoever finishes first either commits or kills. There is no quiet wrong-bytes case.

## Resolution: requirements, not exact pins

Build declarations specify toolchains as semantic ranges, not exact builds: `msvc >= 17.0`, `macos_sdk >= 13.0`, `rustc = 1.91`. The runtime resolves each requirement to a concrete toolchain identity by querying available inventories:

- **Vixen-controlled artifacts.** What versions can we fetch?
- **Local discovery** (when the build runs on the user's machine, not a Vixen executor). What does the host have that satisfies?

Picks one toolchain identity per requirement. Different builds may resolve the same requirement differently — that's fine; they end up in different cache buckets and never mix.

## Coherence within a build, not blanket per-build

There is no single "all local vs all remote" switch at the top of a build. Resolution is **per-toolchain**: each requirement resolves against the union of inventories and picks whichever satisfies.

The non-mixing constraint is at the *artifact coherence* level: any one artifact's toolchain set must be internally compatible. You can't link half a crate's transitive deps against the macOS 13 SDK and half against the 15 SDK — the ABI doesn't match. So a single artifact commits to one coherent toolchain set. Fan-out across executors (Linux × macOS × Windows × arm/x86 for a cross-compile matrix) is fine because each branch of the fan-out is internally coherent.

If the same `Cargo.toml` would resolve differently locally vs against Vixen executors (because available toolchain versions differ between local discovery and remote inventory), the cache buckets differ — the lockfile-as-output for "build against local toolchains" is different from "build against remote toolchains." Same shape as cargo's existing target-specific lockfile mechanism.

## Vixen-compute CI collapses discovery to declared identity

When the build runs on a Vixen-controlled executor (Tier 2 and onward), discovery is mostly vestigial. We know exactly what's on our executors — we built the image. The "obligation" for every toolchain on a Vixen executor is a single declared-identity token: "this is executor image v7, manifest hash X, signed by us." See [Cache and trust](/cache-and-trust/) on declared-identity-collapses-the-read-set.

Local builds are where the discovery machinery earns its keep. The user's machine is heterogeneous and out of our control; discovery + obligations + poison-and-redispatch is the mechanism that makes reuse safe.

## Lockfiles are build outputs, not input mutations

A consequence of treating resolution as a build-graph node like any other.

The resolution operation is:

```
(Cargo.toml, registry view, resolver version, toolchain pins) → solve → Cargo.lock
```

`Cargo.lock` is an output. The fact that it lands at the same on-disk path as a previous lockfile is a file-system convention, not a semantic mutation. When the resolution node *is* run, the cache stores `(inputs → Cargo.lock)`; same inputs → same lockfile → instant.

But resolution is **demand-gated, not triggered by registry drift.** On an ordinary build the committed `Cargo.lock` *is* the solution — `@vixen/cargo` reads it, checks it still satisfies `Cargo.toml`, and if so never forces the resolution node at all. A new version published on crates.io does **not** re-resolve; the lock pins what you build. The registry view enters only the resolution node's key, and an ordinary build never demands that node.

The node is forced only when the lock can't serve as the solution — it's missing, the manifest changed in a way the lock can't satisfy, or the user asked. In those cases the prior lock is fed back in as an **input preference** (keep existing pins, change only what must), not the authoritative solution. `cargo update` is exactly that forced re-resolution with the preference relaxed: bare `update` ignores the prior lock as a solution and resolves to latest compatible; `update -p foo` keeps every pin except `foo`. No special primitive — `@vixen/cargo` chooses whether to force the node and how strongly to weight the prior lock, and the resolver intrinsic accepts that prior solution as a preference input.

Implications:

- No "stale lockfile" guessing. The cache decides.
- Lockfiles cross-target cleanly. Workspaces targeting different toolchain sets produce different lockfiles; both are valid outputs of the same shape of operation.
- Package-version pins fit the same model. A workspace also depends on specific Vixen-package versions (`@vixen/rust@2.4.1`); those pins are inputs to the resolution operation; they appear in the lockfile output. See [Packages and publishing](/packages-and-publishing/).
- If a build changes your checked-in `Cargo.lock`, the diff is honest: it means the lock could no longer serve as the solution (the manifest changed, or you ran `cargo update`) — not that the registry moved underneath a plain build.

## PubGrub solves both cargo and Vixen package resolution

One constraint solver, two domains.

PubGrub is a generic SAT-shaped algorithm. The runtime ships it as an intrinsic. The domain knowledge — "what counts as compatible," "what dep relationships mean" — is supplied by the caller:

- **Vixen package resolution.** A small core Vixen module distributed with the runtime knows Vixen's package semantics and calls PubGrub.
- **Cargo crate resolution.** `@vixen/rust` knows cargo's domain model (workspaces, features, `[patch]`, target-specific deps, version selection) and calls PubGrub the same way.

Same engine, same diagnostics, different domain rules. When `@vixen/python` or `@vixen/node` arrives, they declare their semver model and call into the same intrinsic.

## cfg and target-table matching (must-cover, open)

A drop-in cargo replacement **must** handle cargo's target-conditional dependencies: `[target.x86_64-unknown-linux-gnu.dependencies]` (explicit triple) and `[target.'cfg(...)'.dependencies]` with the full `cfg` mini-language — `cfg(unix)`, `cfg(target_os = "linux")`, `cfg(all(...))`, `any`, `not`, and the derived flags (`unix`, `windows`) that aren't literally a triple field. This is non-negotiable for compat; the exact mechanism is **not yet settled**.

What is clear about where it sits:

- **It is resolution-time domain logic in `@vixen/rust`, not a fleet-routing constraint.** You don't route executors by `cfg`; you select which deps apply for a resolved target. So it is *not* a language-level [constraint operator](/vixen-language/#constraint-expressions) and does not need to be registry-indexable. `@vixen/rust` owns cargo's `cfg` semantics the same way it owns features and `[patch]`.
- **The cfg attribute set for a target is data the runtime can supply.** `rustc --print cfg --target <triple>` yields the concrete cfg key/values and bare flags for that target — an `exec` whose observer returns the parsed set. Given that set, evaluating a `cfg(...)` expression is ordinary boolean logic (`all`/`any`/`not`, key-equality, flag membership) expressible with the built-in operators over plain data.
- **Explicit-triple tables** are a string match on the resolved target triple; those *do* fall out of ordinary projection + comparison.

Open: how `@vixen/rust` parses the `cfg(...)` mini-language (its own small parser vs. a runtime intrinsic), how the per-target cfg set is acquired and cached (declared-identity on a Vixen executor vs. discovered on a customer host), and how cfg-conditional edges thread into the PubGrub solve so a target resolves to the same dependency set cargo would pick — functional equivalence, not a byte-identical lockfile. Tracked here as a compat blocker for the cargo path.

## Bootstrap is clean

The "tiny core" Vixen package that defines Vixen-package-resolution semantics ships embedded with the runtime. Resolving Vixen packages doesn't require already having resolved them — the core is just there. PubGrub is an intrinsic. No chicken-and-egg.

## Hermetic inputs don't need source hashing on the client

A consequence worth being explicit about for Tier 1.

When every dep is a hermetic input (crates.io entries), the cargo fork does *not* hash the source files of those deps to derive the cache key. The identity is the descriptor — `(name, version, registry-hash)` plus compile args plus toolchain identity. crates.io's version-immutability guarantee means the descriptor *is* the identity.

The fork derives keys for every unit using descriptors, sends the whole batch to ccc-backend in one RPC, gets back the hit/miss partition. No per-source-file hashing on the client side. (Trusted builders hash the bytes when they backfill, because they need to verify what they pulled from crates.io.)

Private crates and workspace-local crates fall outside this — they have no registry hash, no immutable identity. Tier 1 doesn't cache them; they rebuild locally on miss. Tier 2's private-cache story (see [Cache and trust](/cache-and-trust/)) handles them when it ships.
