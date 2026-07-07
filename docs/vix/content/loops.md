+++
title = "Loops"
weight = 5

[extra]
updated = "2026-07-08"
+++

vix has no `while`, no `for`, no `loop`. Iteration is recursion — and
**self-recursion in tail position is a real loop**: the compiler turns it
into a same-frame backward jump, not a function call, not a new demand.

```vix
fn drain(tokens: [Int], stack: [Int]) -> [Int] {
    match step(tokens, stack) {
        Continue(rest, next) => drain(rest, next),   // tail call → a jump
        Done => stack,                               // loop exit
    }
}
```

`drain(rest, next)` compiles to: evaluate the new arguments into fresh temps,
overwrite the parameter slots from those temps, jump back to the top of the
function body. No stack growth — a hundred-thousand-iteration loop is
measured and fine — no per-iteration call overhead, and critically, **no
per-iteration demand**: the whole loop is *one* memoized computation, spawned
once, and its accumulators stay [molten](/vix/molten/) from entry to exit,
mutated in place, frozen exactly once when the loop's answer leaves the
function.

## Why this needed its own mechanism

Every source-level vix call is an INVOKE — a full memo-boundary demand (see
[Functions and calls](/vix/functions/)). Taken completely literally, that
means naive self-recursion could never keep an accumulator molten across
iterations: each recursive call would freeze the accumulator into the store,
re-hash it, and spawn a fresh computation, every single step. That's
precisely the gap the language closed by giving *tail* self-calls a different
lowering — an ordinary backward jump inside the same frame — while leaving
every other call, including non-tail self-recursion, on the INVOKE path
where memoization is exactly what you want.

## The rules are syntactic and checkable

- **Tail position** means: the last expression of the function body, or of a
  `match` arm (or block) that is itself in tail position. `1 + drain(...)` is
  not tail — the `+` still needs the result — but `drain(...)` as an entire
  match arm's body is, including nested inside further `match` arms. This is
  what makes the idiomatic shape above (`match ... { Continue(...) =>
  drain(...), Done => stack }`) loop at all: tail position threads through
  `match`.
- **Self only, today.** The call must target the exact enclosing function —
  not a same-named function in another scope, not a polymorphic dispatch.
  Mutual tail recursion (two functions jumping to each other as a state
  machine) isn't covered yet; it's designed and queued.
- **Same shapes.** New argument values are coerced against the parameters'
  declared schemas before the jump is emitted; if a coercion doesn't produce
  an exact schema match — meaning the accumulator's *shape* would need to
  change across iterations — the compiler silently falls back to an ordinary
  INVOKE call for that call site instead of erroring. Correct, just not
  accelerated: a backward jump reuses the same frame slots every iteration,
  and a slot can't hold two different schemas.

Argument evaluation is staged in two passes specifically to make
argument-swapping recursion safe: every new argument is computed into a fresh
temporary *before* any parameter slot is overwritten, so `f(b, a)`-style
swaps can't clobber a value that a later argument expression still needs to
read.

## Everything else stays a demand, on purpose

A non-tail self-call — tree recursion like `solve(left) + solve(right)`, or
`fib(n - 1) + fib(n - 2)` — keeps per-call memoization, which is exactly what
tree recursion wants: shared subproblems get shared answers, for free,
because they hit the same memo key. The dividing line is structural and it
lands where it should: **linear iteration loops; branching recursion
memoizes.** See [Functions and calls](/vix/functions/) for the `fib`
walkthrough.

When you write a self-call that *isn't* in tail position, the compiler tells
you — a diagnostic, not an error: `"self-call at ... is a demand boundary
(not tail position) — not looped."` If you meant it (tree recursion), ignore
it. If you meant a hot loop, it just told you exactly why it's slow. See
[Diagnostics](/vix/diagnostics/).

## What's proven, not just claimed

The loop-vs-demand boundary is asserted directly against the machine's event
trace, on both execution lanes (interpreter and JIT), not inferred from
results looking right: one demand and one spawn cover a twelve-step
`countdown` regardless of iteration count; the same loop with a struct-typed
accumulator produces results and read-sets identical to a forced-INVOKE
baseline of the same program (the loop lowering is a behavioral no-op versus
the memoized path it replaces); a 100,000-iteration countdown still completes
with exactly one spawn and one completion; and an array-accumulating
100,000-iteration loop interns its result array exactly once — not once per
push — tying this directly to [molten values](/vix/molten/): the tail loop
is what lets an accumulator stay molten across iterations at all.
