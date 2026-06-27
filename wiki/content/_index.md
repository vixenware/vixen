+++
title = "Vixen"
+++

Spec and design corpus for Vixen. The wiki is the live spec home; Dodeca tracks
requirements and implementation links as that flow lands.

## Two products

Vixen has two products. They share some infrastructure, but customers buy and use
them separately.

- [CCC (Cargo Cloud Cache)](/ccc/) is a Rust CI cache product. It ships as a
  cargo fork through a rustup mirror. Customers keep typing `cargo build`; CCC
  sends cargo's resolved unit graph to `ccc-backend` in one RPC and materialises
  only demanded cache hits.
- [Vixen Build](/vixen-build/) is a build-system product. Customers opt in with
  `vx cargo build`; the runner orchestrates while Vixen executors run the build
  under the sandbox, VFS, package, and cache runtime. Vixen CI is the later
  `.vx` authoring surface.

CCC can ship without the Vixen runtime. Vixen Build uses CCC's cache/trust
lessons, but is a separate product with a separate adoption path.

Start with [Vixen Build](/vixen-build/#settled-decisions) for product decisions.

## Cross-cutting substance

- [Cache and trust](/cache-and-trust/) — disciplines that apply to both products (tightening monotonic, per-cache trust, declared identity). Mechanism differences are noted per product.
- [Architecture and infrastructure](/architecture-and-infrastructure/) — components and where they run.
- [Glossary](/glossary/) — short term definitions.

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
