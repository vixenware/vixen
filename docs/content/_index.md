+++
title = "vixen"
+++

vixen is a general-purpose build system with an emphasis on Rust.

It is built on one idea, applied everywhere: **a build is a question, not a script.**
You ask for an artifact — a binary, a test result, a rendered page — and the machine
computes exactly what that answer needs, remembers everything it computes by
*content*, and observes precisely what every step actually read. Everything else
follows from that:

- **Change a comment — nothing rebuilds.** Identity is content, not timestamps.
- **Change a function — its dependents rebuild, and nothing else.** Dependencies are
  observed at item granularity, not declared at file granularity.
- **Rerun eleven tests, skip 189 — soundly.** Which tests depend on what is computed
  from the artifacts themselves, not guessed from coverage.
- **Trust the cache.** Processes run hermetically: undeclared reads are impossible,
  so a cache hit is a proof, not a hope.

vixen is powered by **vix**, a demand-driven language in which the entire build
logic is written — including vixen's own understanding of Cargo. You can read it,
extend it, and debug it like any other code, because the machine underneath is
relentlessly observable: demand traces, real DWARF for JIT'd code, profilers that
attribute every nanosecond.

## Where to go

- **[Concepts](@/concepts/_index.md)** — the mental model: demand, content
  addressing, molten values, hermetic execution. Read this first; everything else
  is a corollary.
- **[The constellation](@/constellation/_index.md)** — the map of the pieces:
  vix, snark, weavy, taxon, rodin, and friends — what each one is and why it exists.
- **[Using vixen](@/using/_index.md)** — install it, build a workspace, read your
  first receipts.
- **[The vix language](@/vix/_index.md)** — guide and reference for the language
  the builds are written in.
- **[Developing vixen](@/developing/_index.md)** — contribute: the invariants, the
  oracles, the method.
- **[Status](@/status/_index.md)** — what works today, honestly, and what's next.
