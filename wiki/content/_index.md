+++
title = "Vixen"
+++

Spec and design corpus for Vixen. The wiki is the live spec home.

## Two products

- [CCC (Cargo Cloud Cache)](/ccc/) — Rust CI cache. Ships as cargo; no Vixen
  runtime, no `.vx`.
- [Vixen Build](/vixen-build/) — build system. Ships as `vx cargo build`;
  executors run builds under the Vixen runtime, sandbox, VFS, package, and cache
  model.

Start with [Vixen Build](/vixen-build/#settled-decisions) for product decisions.

## Shared Docs

- [Cache and trust](/cache-and-trust/)
- [Architecture and infrastructure](/architecture-and-infrastructure/)
- [Glossary](/glossary/)

## Vixen Build subsystem docs

- [Vixen language](/vixen-language/)
- [Open language kernel strategy](/open-language-kernel-strategy/)
- [Build and resolution](/build-and-resolution/)
- [Packages and publishing](/packages-and-publishing/)
- [Executor abilities](/executor-abilities/)
- [Epic: vx cargo build, end-to-end](/vx-cargo-build-trace/)

## Tasks

Open work the docs reference. Each is a `Task` with status, priority, and tier tags.

- [Design cache key + sandbox together](/cache-key-and-sandbox-coupled-design/) — high, Tier 2.
- [Implement streams as the first Tier-2 primitive](/streams-first-primitive/) — high, Tier 2.
- [Decide the process model](/process-as-first-class-value/) — medium, Tier 2.
- [Design and implement Windows sandbox (AppContainer)](/windows-sandbox-direction/) — high, Tier 2.
- [Decide how private crates get cached](/private-crate-caching/) — medium, Tier 2.
- [Design vx-store regional replication topology](/vx-store-regional-replication/) — medium, Tier 1/2.
- [Bytecode + JIT exploration for hot Vixen code](/bytecode-and-jit-exploration/) — low, Tier 2/3.

## Conventions

- Two page types: `Doc` (synthesised explanation, organised for reading) and `Task` (a thing to do, with status and blocking edges).
- Frontmatter is TOML (`+++`). Page type lives in `[extra].type`. Tier tags on docs via `[extra].tiers`. Task status in `[extra].status`; `superseded` is valid, but superseded pages should be short tombstones pointing at the current model, not long contradictory archives.
- Cross-references in Markdown use explicit `/path/` URLs to other docs. Frontmatter cross-references use route slugs declared with `@link`.
- Substance — visions, decisions, milestone narratives — is human-authored. Agents do scaffolding, search, transcription, validation.
