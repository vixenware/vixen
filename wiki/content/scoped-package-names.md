+++
title = "Vixen package names are scoped, npm-style"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["packaging", "naming", "language"]
+++

## Context

cargo's flat global namespace is a known design mistake. The Rust community has been working around it for years — projects prefix every crate name with their org or initiative (`tokio-*`, `tower-*`, `bevy_*`, etc.) precisely because there's no namespacing. Squatting is common, ownership is invisible from the name, and the first-come-first-served model rewards landgrabs.

We're not going to repeat the mistake.

## Decision

Vixen package names are **scoped**, npm-style. Every package lives under a scope owned by a publisher:

- `@vixen/rust` — first-party.
- `@vixen/c` — first-party.
- `@bearcove/some-package` — published by the bearcove org.
- `@community-author/some-package` — published by a community maintainer.

No bare names. No legacy compat with cargo's flat model.

## Consequences

- **Ownership is visible in the name.** A consumer reading a manifest's dependency list immediately sees who maintains each package. No googling required.
- **Squatting is impossible by construction.** You can only publish under a scope you've claimed. Two unrelated parties picking the same package name is fine as long as their scopes differ.
- **First-party packages are unambiguous.** `@vixen/*` is reserved for the Vixen team. Anyone seeing `@vixen/rust` knows it's not community-built.
- **OIDC trusted publishing aligns with scopes.** Scopes are owned by accounts; accounts are tied to CI provenance via OIDC. Publishing to `@bearcove/foo` requires presenting an OIDC token that maps to the bearcove scope. See [[Package publish uses OIDC trusted publishing only]].
- **Search and discovery improve.** Indexing by scope, finding "what does this org publish?", filtering by trusted scopes — all easier than flat-name search.
- **No migration path from cargo.** Vixen packages are a different namespace from cargo crates. `vixen-rust` (the Vixen package that *knows about cargo*) is `@vixen/rust`. The `serde` crate on crates.io has no Vixen-package counterpart by default; if someone wants to vendor `serde`'s build logic as a Vixen package, they publish e.g. `@dtolnay/serde-build` (or wherever) under a scope they own.

## Related

- [[Package management for Vixen]]
- [[Package publish uses OIDC trusted publishing only]]
- [[Store gains a retention dimension for packages]]
