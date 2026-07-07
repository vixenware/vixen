+++
title = "Observed read-sets"
weight = 3
+++

When a computation runs, vixen records what it *actually read* — not what a
configuration file claims it depends on. This record is its **read-set**, and it
is the difference between invalidation that is exact and invalidation that is
folklore.

The distinction that matters is **observed versus declared**:

- A *declared* dependency list is a promise made by a human (or a build file),
  checked by nobody. Declare too much and you rebuild constantly for no reason.
  Declare too little and you get stale artifacts — the build system lies to you
  with a straight face. Every timestamp-based and most graph-based build systems
  live on declared dependencies.
- An *observed* read-set is evidence. The machine watched the computation read
  those things. Nothing else can invalidate it; all of it can.

The two failure modes are not symmetric. Over-invalidation wastes time;
**under-invalidation produces wrong builds** — and for a build system, silent
wrongness is the cardinal sin. Observation removes the under-invalidation case
structurally instead of socially.

## Granularity is where the payoff lives

Read-sets in vixen are fine-grained. A computation that reads one field of a
large value records a dependency on *that field's content* — a projection — not
on the whole value. A parse depends on the byte spans it consumed. A build unit
depends on the interface of its dependency, not the dependency's whole compiled
artifact. The finer the recorded reads, the more surgical the invalidation:
change something nothing read, and *provably* nothing happens.

This is also the mechanism behind test selection: a test's read-set —
predicted from the artifact's own reference graph, refined by observed
execution — is what decides whether a change can possibly affect it. See
[Test selection](/using/test-selection/).

## The boundary problem

Inside vix code, reads are observed by construction — the machine mediates every
access. The hard part is *external processes*: when `rustc` runs, who watched
what it read? Today the answer is staged: undeclared reads are **confined**
(the sandbox makes them fail loudly, so the declared set can't silently lie),
and the virtual filesystem is built to **observe** every open, upgrading
declared read-sets to observed ones. The honest current state is on the
[Status](/status/) page — this is engineering in motion, and the
design treats "the read-set is a lie" as the bug class to eliminate, not to
manage.
