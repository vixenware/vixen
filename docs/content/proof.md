+++
title = "Proof"
weight = 2
+++

Every big claim on this site, with its evidence, its methodology, and its
scale-honesty in the same breath. Dated 2026-07-07; this page only moves
forward.

## "Only the tests a change can reach rerun" — demonstrated

The mechanism ([test selection](/using/test-selection/)) was validated on real
compiled artifacts: a fixture workspace (library + test binary), analyzed by
walking the object files' relocation graphs with content-hashed function
sections. The measured matrix:

| Edit to the library | Tests invalidated |
|---|---|
| body of a function two tests call | exactly those two |
| body of a *generic* function one test instantiates | exactly that one (caught by content, not call-edges — the case naive graph-walking misses) |
| same-line comment edit | **none** |
| line-shifting comment edit | over-invalidates (known cause: panic-location metadata in the binary; fix identified) — note the direction: *safe* |

Scale honesty: this is a fixture (handful of tests), built to prove the
mechanism end-to-end including its known sharp edge. The "skip 189 of 200"
framing on the front page is this matrix's arithmetic at suite scale, not yet
a measured production run.

## "Resolves exactly as Cargo would" — measured continuously

The resolver is tested differentially: fixtures materialize as *real* Cargo
workspaces, resolved by vixen and by real `cargo`, selections diffed.
**Current: 4 of 5 differential fixtures match.** The fifth exercises
weak-feature unification (`dep?/feature`) — named, public, in progress. The
end-to-end pipeline (manifest → resolve → fetch from real crates.io → build
with real rustc) additionally matches Cargo's own unit graph on its fixtures,
including a build script writing `OUT_DIR` and a proc-macro crate.

## "Fast enough to write builds in a language" — the ladder

vixen's build logic runs in vix, and the obvious objection is speed. It was
measured, brutally, on the resolver's hottest loop (fixture-scale, one
machine, methodology in the dev records):

| Configuration | vs. native reference |
|---|---|
| naive (every step re-hashed) | **7,300× slower** |
| + incremental hashing | 16.8× |
| + in-place reuse (the shipped design) | **≈ parity** on the loop |

A second workload (a parser's inner loop) went 232,000× → 293× in one day of
landing the designed fixes, with the remaining gap attributed and in progress.
The point of publishing the ugly starting numbers: the claims here are
measured, including the ones that were bad.

## "Undeclared reads fail loudly" — demonstrated

The sandboxed executor's test suite includes, permanently: a normal build
round-trips; a process reading an *undeclared* path fails visibly; an
undeclared write is denied. macOS (Seatbelt) today, Linux (Landlock) staged.

## What's not here yet

A reproducible public demo bundle ("run these three commands, watch the
receipt diff") ships with the first access wave — the receipts exist today as
test artifacts, and turning them into something you can run and share is
[tracked](/status/). This page will link it the day it lands.
