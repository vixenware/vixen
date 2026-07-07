+++
title = "Demand-driven evaluation"
+++

Most build systems are schedulers: they take a dependency graph, sort it, and
execute it forward. vixen is not a scheduler. It is an **evaluator**: you demand a
value — "the test results for this workspace," "the binary for this crate" — and
the machine works *backward* from that question, computing only what the answer
transitively needs.

This sounds like a philosophical distinction. It is an operational one:

- **Nothing runs unless something needs it.** There is no "build everything, then
  pick what you wanted." An unreferenced target is not skipped — it simply never
  becomes a computation at all.
- **Laziness composes.** If a function takes an expensive argument but only uses
  it in one branch, the expensive argument is only computed when that branch is
  taken. This applies uniformly: to build steps, to parsed files, to fields of
  values.
- **The memo is the scheduler.** When two units depend on the same third unit, no
  coordination is needed: both demand it, the first demand computes it, the second
  hits the memo. Topological ordering falls out of evaluation order; there is no
  sort.

## What a demand looks like

Every demand is a memo key: *(which function, content-hash of its arguments)*.
When you demand `build(workspace)` twice with byte-identical inputs, the second
demand is a lookup, not a computation. When one input changed, the demand
recomputes — and every demand *it* makes recurses through the same test, so
recomputation stops at the first level where content is unchanged. This is
**early cutoff**, and it happens at every edge of the graph, not just the leaves.

## Why this cannot be "added later"

Demand-drivenness is not a caching layer bolted onto an executor. The memo, the
read-set tracking, and the incremental recomputation all *assume* evaluation flows
backward from need. An eager system with a cache in front answers a different
question ("did I already run this?") than a demand system ("what does this answer
need?") — and only the second one lets laziness, memoization, and exact
invalidation compose.

vixen enforces this structurally: the machine emits a trace of every demand,
memo hit, and computation spawn, and the test suite asserts on the *shape of the
trace*, not just results. A change that silently makes evaluation eager fails
tests even when it produces identical outputs. The invariant is guarded by
machinery, not by memory.

## What this means for you

Practically: stop thinking "what does my build run?" and start thinking "what
does my question need?" When something rebuilds that you didn't expect, the trace
answers *which demand pulled it in* — see [Receipts](@/using/receipts.md). When
something *doesn't* rebuild that you expected, that's the system working: the
answer's content couldn't have changed, and the trace can show you why.
