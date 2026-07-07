+++
title = "The constellation"
weight = 2
+++

vixen is not one program; it is a small constellation of tools, each with one job,
composing into the build system. This section is the map — one honest page per
piece: what it is, why it exists instead of an off-the-shelf alternative, and how
it connects to the others.

The short version, top to bottom:

| Layer | Piece | One line |
|---|---|---|
| build system | **vixen** | the product: builds, tests, caching, distribution |
| resolver | **rodin** | dependency resolution (cargo-compatible), written in vix |
| language | **vix** | the demand-driven language builds are written in |
| parsing | **snark** | declarative parsers (tree-sitter reborn), text today, binary next |
| substrate | **weavy** | the execution substrate: memory layout, VM, JIT, debug info |
| types | **taxon** | the shared type vocabulary and content-derived identity |
| reflection | **facet** | Rust reflection — how Rust types enter the system |
| wire | **phon** | serialization: schemas on the wire, cross-language |
| RPC | **vox** | typed RPC with generated clients (TypeScript, Swift, …) |
| storage | **vx-store** | content-addressed persistent storage |
| docs | **dodeca** | this site: content, annotations, search |
| templating | **gingembre** | templating with real provenance |

If you only read three pages, read [vix](/constellation/vix/),
[weavy](/constellation/weavy/), and [taxon](/constellation/taxon/) — the
language, the substrate, and the identity model carry the rest.
