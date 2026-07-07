+++
title = "Loops"
weight = 3
+++

vix has no `while` or `for`. Iteration is recursion — and **self-recursion in
tail position is a real loop**: the compiler turns it into a jump, not a call.

```
fn drain(tokens: [Int], stack: [Int]) -> [Int] {
    match step(tokens, stack) {
        Continue(rest, next) => drain(rest, next),   // tail call → a jump
        Done => stack,                               // loop exit
    }
}
```

That `drain(rest, next)` compiles to: rebind the parameters, jump back to the
top. No stack growth (a hundred-thousand iterations is fine), no per-iteration
call overhead — and crucially, **no per-iteration demand**. The whole loop is
*one* memoized computation; its accumulators stay
[molten](@/vix/molten.md) from entry to exit, mutated in place, frozen once
when the loop's answer leaves.

The rules are syntactic and checkable:

- **Tail position** means: the last expression of the function body, or of a
  match arm / block that is itself in tail position. `1 + drain(...)` is not
  tail (the `+` still needs the result); `drain(...)` as an arm's entire body
  is.
- **Self only** (today): the call must target the enclosing function. Mutual
  tail recursion — two functions jumping to each other as a state machine —
  is designed and queued.
- **Same shapes**: the arguments must match the parameters' schemas (they're
  being rebound in place).

Everything else stays a demand, *on purpose*. A non-tail self-call — say, tree
recursion like `solve(left) + solve(right)` — keeps per-call memoization,
which is exactly what tree recursion wants (shared subproblems get shared
answers). The dividing line is structural and it lands where it should:
**linear iteration loops; branching recursion memoizes.**

When you write a self-call that *isn't* in tail position, the compiler tells
you — a diagnostic, not an error: "self-call at ... is a demand boundary (not
tail position) — not looped." If you meant it (tree recursion), ignore it. If
you were writing a hot loop, it just told you why it's slow.
