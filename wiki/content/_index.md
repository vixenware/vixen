+++
title = "vixen wiki"
+++

Durable home for multi-thread architectural work on Vixen. Substrate is dodeca; substance is human-authored.

## Docs

- [The pitch](/pitch/) — the three-tier product strategy with the corrected Tier 1 mechanism, buyer profiles, pricing, and the full enterprise feature catalogue.
- [Cache and trust](/cache-and-trust/) — cache-key correctness, per-cache trust gates, declared-identity shortcuts, the demand-warmed flow, backfill sandbox discipline.
- [Architecture and infrastructure](/architecture-and-infrastructure/) — components and where they run. Cargo fork, rustup mirror, ccc-backend (two-layer), Store with retention dimension, per-OS VFS+sandbox stack, the trusted-builder fleet (Hetzner Hel1 + US expansion), OIDC auth, build-graph-as-provenance.
- [Vixen language](/vixen-language/) — runtime-vs-Vixen split, the FFI shape (body-absent intrinsics, parser-into-typed-values), the hashable/comparable/serialisable property, kwargs via `with`, effect annotations.
- [Build and resolution](/build-and-resolution/) — hermetic vs discovered inputs, obligations and poison-and-redispatch, per-toolchain resolution, lockfiles-as-outputs, PubGrub for both cargo and Vixen package resolution.
- [Packages and publishing](/packages-and-publishing/) — manifest + entry-point shape, scoped names, Vixen-org-to-external-org identity flow, OIDC-only publish, registry-as-index over content-addressed Store.
- [Glossary](/glossary/) — short term definitions.

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
- Frontmatter is TOML (`+++`). Page type lives in `[extra].type`. Tier tags on docs via `[extra].tiers`. Task status in `[extra].status`.
- Cross-references use either `/path/` URLs to other docs or `[[Page Title]]` wiki-style; both resolve through dodeca.
- Substance — visions, decisions, milestone narratives — is human-authored. Agents do scaffolding, search, transcription, validation.
