+++
title = "Failures, poison, and Result"

[extra]
type = "Doc"
tiers = ["1", "2"]
tags = ["failure", "poison", "result", "errors", "language"]
+++

Vixen has **one failure value in two modes**. The same `Failure` either propagates
(a **poison**) or is caught (`Result.Err`); the `?` suffix switches between them.
This is the language-wide model; [exec](/exec/)'s `run.ok` is its most-used
instance.

## One value, two modes

A **`Failure { err, context }`** is the single failure value:

- **Propagating — a poison.** It detonates when forced and propagates up the
  demand graph to the root, **terminating the build**; the runtime renders the
  report. Nothing downstream of a poisoned value runs.
- **Caught — `Result.Err(Failure)`.** The `?` suffix catches a would-be poison and
  hands it to you as data.

Same value; the *only* difference is whether someone caught it. "Everything in the
poison is in the `Err`" because they are literally the same `Failure`.

## Raising — `fail`

`fail e` raises a `Failure` with `err = e`, **total over any value**: `fail "rustc
failed"` gives `err: String` (no wrapper needed — carrying context was never the
error value's job, see below); `fail proc_error` gives `err: ProcError`. `fail f`
re-raises a caught `Failure`, preserving its `err` + `context`. Stdlib operations
raise on their own failure case — the pervasive "poisons if the array is empty,"
"poisons on invalid UTF-8," etc.

## Catching — the `?` suffix

`expr?` evaluates `expr` in **caught mode**: a poison `expr` would raise becomes
`Result.Err(Failure)`, and success becomes `Ok(v)`. It does not propagate — it
gives you the failure as data. (`?` is the *fallible-variant suffix*; this is what
the variant yields.)

## The error type is an inferred union

`err`'s type is the **anonymous union** `E1 | E2 | …` of the error types that can
reach that `?` — the same structural sum Vixen already uses (`ExecInput = String |
Path | Blob | …`), matched the same way.

- **Concrete when there's one source.** `run.ok` fails only with `ProcError`, so
  `run.ok? : Result[Exit, ProcError]` — match `ProcError` directly.
- **`E1 | E2 | …` for heterogeneous catches** — `?` over a region that can fail
  several ways gives the union; you match the variants.

**Exhaustiveness is the early-warning system.** The union is *inferred from the
call graph* — a **snapshot**, one frame of a moving picture (same honesty as the
[inside-out turn](/runtime-semantics/#the-graph-is-built-at-runtime-not-compiled-once)).
When someone downstream adds a `fail NewError`, the union at your `?` grows to `… |
NewError`, and a once-exhaustive `match` goes **non-exhaustive → the checker flags
it**. You add an arm or write `_`. New failure modes cannot slip past a catcher
silently; `_` ("handle the rest") is the explicit escape. Correctness over
convenience — the right default for a build system.

## `Failure { err, context }` — domain vs. envelope

The split that makes `fail` total without forcing wrappers:

- **`err: E`** — the **domain error** (the inferred union). Pure data, the author's
  concern. A bare `String` is a fine `err`.
- **`context: FailureContext`** — the **uniform runtime envelope**, identical for
  every failure regardless of `E`: the source span, the strand's stack + the
  demand-path that forced it, timing. The runtime fills it at the raise site, and
  the demand-path **accumulates as a `Failure` re-propagates** (so a re-raised
  failure carries the whole chain).

The human-readable report is **derived** from `err` + `context` — never stored as a
pre-formatted string.

## Matching

Plain nested destructure — no new construct:

```vixen
match run.ok? (
  Ok(exit)                                                  => exit.out,
  Err(Failure { err: ProcError { cause: Exited(7) }, .. })  => …,      // normalize: 7 ≡ success
  Err(f)                                                    => fail f, // re-raise, context intact
)
```

(If `Err(Failure { err: … })` gets noisy, a thin sugar letting an `Err` arm narrow
the inner `err` directly is a later nicety — not part of the model.)

## In the runtime

This generalizes **I6 source fidelity** (see [the strand scheduler](/strand-scheduler/)):
the runtime's `Poison` stops being `{ reason: String, span }` and becomes the
structured `Failure` — `err` (a typed value, not a string) + `context` (span +
stack + demand-path). `?` is a **catching force**: forcing a sub-expression under
`?` intercepts a poison from it into `Result.Err` instead of letting it propagate.
The stack/demand-path capture reuses the same backtrace machinery the stall report
already uses, so it is cheap and only paid at a failure's *creation*, not per force.

## Cacheable

A `Failure` is **content-determined** — a function of the inputs, not of timing —
so a caught-and-handled failure caches like any other value (see
[Cache and trust](/cache-and-trust/)). Only *order-exposing* stream consumption
breaks cacheability, and that rule is unchanged.

## Related

- [exec](/exec/) — `run.ok` / `run.ok?`; `ProcError` is the domain `err` of a
  process failure, `Result[Exit, ProcError]` the caught form.
- [The demand-driven runtime](/runtime-semantics/) — poison propagation up the
  demand graph; the node-reference value model.
- [The strand scheduler](/strand-scheduler/) — I6 source fidelity, the
  backtrace/stall machinery `context` reuses.
