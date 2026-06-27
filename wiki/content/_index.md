+++
title = "vixen wiki"
+++

Durable home for multi-thread architectural work on Vixen. Substrate is dodeca; substance is human-authored.

## Two products

We ship two distinct products. They are not tiers of one product. Conflating them produces broken framing fast.

- [The pitch](/pitch/) — one-paragraph overview of each product and how they relate.
- [FAQ: settled decisions](/faq/) — answers to the objections we are done re-litigating (why a new language, why resolution-in-.vx, performance, "isn't this just <X>"). Point people here.
- [CCC (Cargo Cloud Cache)](/ccc/) — the standalone Rust-cache product. Forked cargo, rustup mirror, ccc-backend, one-RPC graph lookup.
- [Vixen Build](/vixen-build/) — the standalone build-system product (with Vixen CI as its public-language surface). `vx cargo build`, Vixen executors, sandbox+VFS, package manager.

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
- Cross-references use either `/path/` URLs to other docs or `[[Page Title]]` wiki-style; both resolve through dodeca.
- Substance — visions, decisions, milestone narratives — is human-authored. Agents do scaffolding, search, transcription, validation.
