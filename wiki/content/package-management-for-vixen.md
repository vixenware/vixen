+++
title = "Package management for Vixen"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
updated = "2026-05-26"
tags = ["language", "packaging", "tier-2", "tier-3"]
+++

[[Domain logic lives in Vixen, not in the runtime|Every domain-specific capability lives in a Vixen package]]: `vixen-rust` knows cargo, `vixen-c` knows clang, etc. Customers eventually pin specific versions; downgrade-on-regression is a real workflow. None of this works without serious package management.

## What's committed

- Vixen packages are the unit of domain-logic versioning.
- "Stay on latest" is the default for most users; backed by clean downgrade-on-regression.
- The package mechanism is alive from Tier 2 onwards (the runtime needs it internally even when customers don't see the language).
- A package manager is in scope, not optional.

## Package shape: manifest + entry point

A package is a directory containing:

```
package-name/
├── package.styx       # manifest
├── main.vx            # entry point module
└── …additional .vx files referenced from main.vx
```

The manifest is a styx file validated against a schema (same machinery as the wiki frontmatter), with at minimum:

- `id` — package name
- `version` — semver
- `description` — one-liner
- `license` — SPDX identifier
- `authors`
- `dependencies` — other Vixen packages with version constraints
- `min-runtime-version` — minimum Vixen runtime version the package's intrinsic usage requires

`main.vx` is the single entry-point module; it re-exports anything else the package wants to surface. No multi-entry-point complexity on day one.

## Resolution

[[PubGrub solves both cargo and Vixen package resolution|PubGrub does the constraint-solving]] — same intrinsic the cargo resolver uses, called from a small core Vixen module that ships with the runtime. The core defines Vixen's resolution semantics; from there, packages like `vixen-rust` define resolution semantics for their domains.

The user-facing UX is cargo-shaped: semver ranges, a lockfile recording the chosen versions, `update` as the explicit re-resolution command. The lockfile is [[Lockfiles are build outputs, not input mutations|a build output]] like every other artifact.

## What's still TBD

- **Registry shape.** One central Vixen-operated registry, federated (per-org, per-customer), per-package URLs (Cargo-style alt registries), or a content-addressed protocol.
- **Wire / fetch protocol.** How a package's contents land in Store. Same shape as the existing artifact path, or a separate package-distribution channel?
- **Naming and identity.** Flat names (`vixen-rust`), scoped (`@vixen/rust`), or reserved prefixes for first-party packages?
- **Security model.** Signed packages? Trusted publishers? Reproducible-builds quorum across multiple signers?
- **Mirroring.** Like [[No crates.io proxy at Tier 1|the crates.io discussion]] — do we mirror at all, defer, or rely on the upstream registry's availability?

## What unblocks resolution

The format and resolver are settled; the registry / naming / wire are next. The package manager needs to exist before Tier 2 ships, because Tier 2 itself depends on `vixen-rust` being a package.

## Related

- [[Domain logic lives in Vixen, not in the runtime]]
- [[PubGrub solves both cargo and Vixen package resolution]]
- [[Lockfiles are build outputs, not input mutations]]
- [[Vixen language stays internal until Tier 3]] — the *language* stays internal but the package mechanism doesn't.
- [[Tier 2 — Vixen Build]]
- [[Tier 3 — Vixen CI]]
