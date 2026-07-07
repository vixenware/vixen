+++
title = "Functions and calls"
weight = 2
type = "reference"
updated = "2026-07-08"
+++

A vix function looks ordinary:

```vix
fn compat_of(v: Version) -> CompatClass {
    match v.major != 0 {
        true => CompatClass::Major(v.major),
        false => match v.minor != 0 {
            true => CompatClass::Minor(v.minor),
            false => CompatClass::Patch(v.patch),
        },
    }
}
```

Calling it is not ordinary. Every source-level vix call is an **INVOKE** — a
demand boundary, full stop. The machine builds a memo key from the function's
identity (a content-addressed hash of its own canonical AST, so editing a
function's body changes its identity) and the *content* of its arguments,
checks whether this exact question has been answered, and either returns the
existing answer or computes — once, recording everything the computation
reads along the way.

This gives every vix function three properties you would normally have to
engineer by hand:

**Automatic memoization.** Two calls with content-identical arguments cost one
computation. This composes through the call graph — a diamond dependency is
computed once, not twice, with no coordination between the two callers that
happen to want it.

**Laziness.** Values are computed when demanded, not when mentioned. Passing
an expensive value to a function that may not use it doesn't compute it;
taking the branch that needs it does. This is checked structurally, not just
claimed: the differential test corpus asserts that an untaken match arm's
computations *never appear in the trace* — not "ran but wasn't read," genuinely
absent as an event.

**Exact re-evaluation.** Because each call's reads were recorded, changing an
input re-runs precisely the calls whose read-sets are touched. Incremental
builds are this property applied to the whole program: a comment edit changes
nothing's content hash, so nothing re-runs; a dependency bump only re-runs the
calls that actually read the changed value.

## Tree recursion keeps its memo — deliberately

`fib(n - 1) + fib(n - 2)` is not tail position (both calls are operands of
`+`, there's work — the addition — after each returns), so each call is a
full INVOKE: a real memo boundary. That's not a missed optimization, it's the
correct behavior for branching recursion, and the machine measures it:
`fib(20)`'s naive call tree has 13,530 nodes, but the driver's memo on
`(fn, args)` collapses it to 21 spawns — one per distinct `n` in `0..=20` —
because every repeated sub-problem hits the same memo key. This is the
resolver's actual shape too: `rodin`'s search branches on package choices,
and each branch is a genuinely distinct sub-problem worth caching, not a loop
step to optimize away.

Contrast a *linear* accumulator, where each step depends only on the previous
one and there's no shared-subproblem structure to exploit — that shape wants
a loop instead of a memo boundary per step, which is exactly what
[tail position](/vix/loops/) is for. **The dividing line is structural: linear
iteration loops, branching recursion memoizes**, and the compiler tells you
which one it thinks your code is (see [Diagnostics](/vix/diagnostics/)).

## Consequences for how you write code

Mostly: **write the naive thing.** The instinct to hoist common
subexpressions, cache intermediate results, or thread state through to avoid
recomputation is usually the machine's job now — a helper called from twelve
places with the same arguments runs once, memoized, with no coordination code
anywhere.

The judgment that remains is **granularity**: every function boundary is a
memo boundary, and identity has a cost — arguments crossing a boundary are
frozen (hashed) to build the key, and composite values crossing a boundary
stop being molten (see [Molten values](/vix/molten/)) and become
content-addressed store entries. Fine-grained functions over huge arguments
churn identity for no reuse benefit; coarse monoliths under-memoize and can't
share partial work. The working rule: put function boundaries where *reuse*
is plausible — "parse this manifest," "resolve this workspace," "derive this
unit" — and keep hot interiors inside one function, where
[loops](/vix/loops/) and [molten values](/vix/molten/) run at machine speed
with no identity cost at all.

## What a call actually does, mechanically

This is worth naming precisely because it's the hinge the rest of the
language turns on (see [Demand semantics](/vix/demand/) for the full model):
a `Call` node demands each of its arguments' *identity* — jointly, as one
batch, which is also the memo key's support (aggregation unit = memo unit).
The key is `(function's closure hash, argument hashes)`; a hit returns the
already-computed node with no new work; a miss expands the function body as a
new demand-driven computation, recording every read as it goes. Nothing about
this is visible from vix source — you write the function, the machine does
the rest — but knowing the shape explains why granularity matters and why
tree recursion memoizes for free.
