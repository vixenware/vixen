+++
title = "Redesign · Demand-driven: the invariant, enforced not remembered"
+++

# Demand-driven — the invariant that can't be retrofitted

This is not one of the fixes. It's the property every fix must not break, and the
one most likely to be silently dropped — so it gets its own doc, and an
enforcement mechanism that does **not** depend on anyone (me) holding it in mind.

## What it means, precisely

vix is a content-addressed **incremental demand machine**. Computation flows
*backward from a demand for a result*: nothing runs unless something transitively
needs it. The machine **pulls** a value, **memoizes** what it computes keyed by
content, **records what each computation read** (projection read-sets), and
**recomputes only what a changed read touched**. Eager evaluation — compute
everything forward, regardless of need — is the opposite, and it is not a "style
choice": the memo, the read-sets, the incremental recompute, and the projection
system exist *only because* evaluation is demand-driven. Strip the demand and all
of that machinery is dead weight solving a problem that no longer exists. "Build
it eager, add laziness later" is not a refactor; it's a different program wearing
the same type names.

## Why it can't be a memory obligation

Every fix in this set is a chance to reach for an eager shortcut because eager
goes green faster. And here's the trap: **the corpus passing does not prove
demand-driven-ness.** A resolve producing the right lockfile says nothing about
*whether it was computed lazily*. An eager reimplementation can produce identical
results and pass every result-assertion. So "keep the corpus green" — the
guardrail for the other fixes — is **not sufficient** for this one. We need a
guardrail that observes *how* the result was computed, not just what it was.

## The tripwire: assert on the demand trace

The machine already emits its own demand behavior as `DriveEvent`s
(`driver.rs:238`): `Demanded`, `MemoHit`, `MemoProjectionHit { verified }`,
`MemoSemanticHit`, `Spawned`, `ParkedOn`, `Completed`, `SpawnedInvocation`,
`StoreAlloc { deduped }`. That stream *is* the observable signature of
demand-driven evaluation, and it already backs tests like
`warm_demand_spawns_nothing` (doc 90: seed memo → demand → `MemoHit`, **zero
`Spawned`**) and `render_result_..._never_forces_pending_values`
(`lower.rs:11083`).

That is the enforcement model for the whole redesign:

- **Behavioral guardrail** (all fixes): the corpus stays green — same results.
- **Demand guardrail** (this invariant): a set of **trace assertions** — spawn
  counts, memo-hit counts, what was demanded vs. what was computed, what forced a
  pending value — that go **red the instant** a fix computes something that
  wasn't demanded, or spawns where it should have hit the memo. Eager substitution
  changes the `DriveEvent` stream even when it doesn't change the result, so the
  trace catches it *at the first offending test*, not at test 200.

The discipline, concretely: **before touching a demand-sensitive subsystem,
confirm a trace assertion covers its demand behavior; if none exists, add it
first.** The tripwire lives in the diff against the event stream, not in anyone's
recollection.

## Which fixes are demand-sensitive (and how)

- **Hashing + flesh (2)** — *aligned, but sensitive.* The whole point (don't
  intern in the hot loop) is a laziness/perf property. Risk: incremental hashing
  or an eager intern could force materialization. Trace guard: intern/StoreAlloc
  counts on the solver path.
- **Reflection / pull accessors (3)** — *sensitive.* Pull is demand-aligned
  (materialize nothing until asked), but an accessor that accidentally *forces a
  pending value* while merely inspecting would break demand. There is already an
  invariant here (`..._never_forces_pending_values`) the new accessors must keep.
  Trace guard: assert inspection produces zero `Spawned`/no forcing.
- **Layout/ABI + store on weavy (5)** — *sensitive.* Changing when/how values
  materialize or intern touches the demand/memo boundary. Trace guard: memo-hit
  and dedup (`StoreAlloc { deduped }`) counts unchanged.
- **Parsers → Snark/vix (6)** — *sensitive.* Parsing today is a *projection*
  (demanded, read-set-tracked, doc 90). Snark/vix parsers must lower to the same
  demand/projection shape, not eagerly parse whole artifacts. Trace guard:
  projection read-sets on artifact access.
- **Host-fn registration (1)** — *mostly orthogonal* (mechanism, not evaluation
  strategy) — **except** the `projection_reads` buffer drained after `burst`
  (`driver.rs:4693`) is part of read-set tracking; the `HostCtx` refactor must
  preserve that draining exactly. Trace guard: read-sets identical before/after.
- **Typed schemas + newtypes (4)** — *orthogonal.* Typing, not evaluation. Lowest
  demand risk.

## The one thing to build if it's missing

Audit whether the trace assertions actually **cover** each demand-sensitive
surface above. Where the corpus only checks results (not the trace), the eager
shortcut is invisible and my prediction-of-myself comes true. Closing those
coverage gaps — a spawn/memo-hit assertion on each sensitive path — is the
concrete, one-time investment that converts "demand-driven" from something I have
to remember into something the test suite refuses to let me break.
