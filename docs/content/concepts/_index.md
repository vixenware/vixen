+++
title = "Concepts"
+++

Five ideas explain everything vixen does. They are not independent features — each
is a consequence of the previous one. Read them in order and the rest of this site
becomes predictable: whenever you wonder "how does vixen handle X?", the answer is
almost always "the same way it handles everything," specialized to X.

1. **[Demand-driven evaluation](@/concepts/demand.md)** — nothing runs unless
   something transitively needs it. The build is a question flowing backward, not
   a script flowing forward.
2. **[Content addressing](@/concepts/content.md)** — every value, artifact, and
   computation is identified by a hash of what it *is*, not where or when it was
   made. Memoization, deduplication, and distributed caching stop being features
   and become arithmetic.
3. **[Observed read-sets](@/concepts/read-sets.md)** — the machine records what
   each computation *actually read*, at fine granularity. Invalidation is exact:
   what you didn't read can't dirty you.
4. **[Molten and frozen values](@/concepts/molten.md)** — inside a computation,
   values are hot and mutable; identity crystallizes only at the boundaries where
   the machine needs it. You write functional code; it runs like imperative code.
5. **[Hermetic execution](@/concepts/hermetic.md)** — external processes (like
   rustc) run inside a sandbox over a virtual filesystem, so their reads are
   confined and observed too. This is what makes a cache hit *sound* rather than
   optimistic.

A sixth page describes how these run in practice across machines:
**[Daemons, runners, and services](@/concepts/services.md)**.
