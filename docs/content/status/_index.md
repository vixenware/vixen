+++
title = "Status"
+++

The honest page. Updated as things land, dated so you can tell how stale it is.

## What works today (2026-07-07)

The **vertical slice** is real: given a Cargo workspace fixture, vixen — running
entirely through vix — resolves dependency versions (rodin), fetches crates from
the real crates.io by checksum, extracts them, plans the build, and drives real
`rustc` invocations (including build scripts and proc-macros), producing a unit
graph that is verified against Cargo's own. The resolver currently matches
`cargo tree` on 4 of 5 differential fixtures; the fifth exercises weak-feature
unification, which is the next resolver milestone.

The **language** grew significantly today: full semver version precedence,
functional-in-place updates (`let xs = xs.push(x)` runs as mutation when
provably invisible), and real loops (tail recursion compiles to a jump — a
100,000-iteration accumulator loop runs with zero per-iteration overhead where
yesterday it couldn't be expressed at all).

The **measurements** behind the design are public in spirit and will be published
here: the resolver's hot loop went from 7,300× slower than the retired Rust
reference to parity-with-reuse in one day of landing the pieces the feasibility
studies had designed.

## What's next

- **Feature unification** in the resolver (the fifth fixture).
- **The real-workspace ladder**: build `taxon` (trivial), then `facet-core`
  (build script), then `facet` (proc-macros + features) end to end.
- **Typed schemas throughout the machine** (in flight): the internal migration
  from string-keyed schemas to content-derived identity — invisible to users,
  load-bearing for everything after it.
- **Hermetic execution, stage two**: from *confining* undeclared reads (done, macOS)
  to *observing* all reads through the virtual filesystem.

## Tiers

Early access lands in waves. This site tags pages with the tier they require;
untagged pages are general. (Tier definitions land here when the first wave
goes out.)
