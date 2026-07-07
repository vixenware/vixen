+++
title = "Machine redesign — per-problem analyses"
+++

# Machine redesign — per-problem analyses

Deep, individual analysis of each major problem in the vix machine, for Amos to
approve **before** any code changes. Each doc is concrete: what to **change**, what
to **add**, what to **remove**, and **how it would work** — grounded in the actual
code, with blast radius, an incremental corpus-green sequence, risks, and open
questions.

The parent inventory (verdicts V1–V10, what weavy/phon/vx-store provide) is at
[machine-redesign](/machine-redesign/).

## Start here

- [**Synthesis**](/redesign/synthesis/) — what connects the six, the one keystone
  under four of them (containers are hand-encoded instead of declared), the
  sequencing constraints, and the six decisions that are yours. Read this first.

## The invariant

- [0 · Demand-driven, enforced not remembered](/redesign/0-demand-driven/) — not a
  fix; the property every fix must not break, and the trace-assertion tripwire that
  catches eager substitution the instant it happens.

## The fixes (one per major problem)

| # | Problem | Verdicts | Status |
|---|---------|----------|--------|
| [1](/redesign/1-host-registration/) | Host-fn registration + typed frame path | V4, V7 | ✅ done |
| [2](/redesign/2-hashing-flesh/) | Hashing + flesh (intern / perf axis) | V2 | ✅ done |
| [3](/redesign/3-reflection/) | Reflection — kill the `RenderedValue` god-enum | V10 | ✅ done |
| [4](/redesign/4-typed-schemas/) | Typed schemas + newtypes | V3, V6 | ✅ done |
| [5](/redesign/5-layout-abi/) | Layout/ABI + the store on weavy | V1 | ✅ done |
| [6](/redesign/6-parsers-snark/) | Parsers out of the language (Snark-for-binary) | V5, V8 | ✅ done |

All seven analyses are written and grounded (`file:line` throughout). **Nothing is
implemented** — this set is for your approval, doc by doc. The site is annotatable
(`--dev-editor amos`); mark them up and I react.

## What landed (2026-07-07)

Retrospective docs, a different kind from the six above: not proposals, but
grounded records of code that's already merged to `rodin`, with tests.

- [7 · Molten reuse](/redesign/7-molten-reuse/) — functional-but-in-place
  mutation semantics, the consuming-move rule, and the differential oracle
  that proves reuse is unobservable.
- [8 · Intra-demand loops](/redesign/8-intra-demand-loops/) — self-tail-calls
  lowered to backward jumps, so a loop costs one demand instead of one INVOKE
  per iteration.
- [9 · Road-2 evidence](/redesign/9-road-2-evidence/) — the measurements that
  settled hot loops living in vix rather than behind FFI.
