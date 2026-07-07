+++
title = "Functions and demand"
weight = 2
+++

A vix function looks ordinary:

```
pub fn lock_package(lock: Doc, name: String) -> Doc {
    ...
}
```

Calling it is not ordinary. A call is a **demand**: the machine builds a memo
key from the function's identity and the *content* of its arguments, checks
whether this exact question has been answered, and either returns the existing
answer or computes — once, recording everything the computation reads.

This gives vix functions three properties you'd normally have to engineer:

**Automatic memoization.** Two calls with content-identical arguments cost one
computation. This composes through the call graph — a diamond dependency is
computed once, not twice, with no coordination.

**Laziness.** Values are computed when demanded, not when mentioned. Passing
an expensive value to a function that may not use it doesn't compute it;
taking the branch that needs it does. The machine's own tests assert this
structurally: an untaken match arm's computations *never appear in the trace*.

**Exact re-evaluation.** Because each call's reads were recorded, changing an
input re-runs precisely the calls whose read-sets are touched. Incremental
builds are this property applied to the whole program.

## Consequences for how you write code

Mostly: **write the naive thing.** The instinct to hoist common subexpressions,
cache intermediate results, or thread state through to avoid recomputation is
usually the machine's job now — a helper called from twelve places with the
same arguments runs once.

The judgment that remains is **granularity**: every function boundary is a
memo boundary, and identity has a cost (arguments crossing a boundary are
frozen — hashed — to make the key). Fine-grained functions over huge arguments
churn identity; coarse monoliths under-memoize. The working rule: make
function boundaries where *reuse* is plausible — "parse this manifest,"
"resolve this workspace" — and keep hot interiors inside one function, where
[loops](/vix/loops/) and [molten values](/vix/molten/) run at machine
speed with no identity cost at all.
