+++
title = "Molten and frozen values"
+++

Content addressing has an apparent tax: if every value's identity is a hash of
its contents, then every modification makes a "new value" — and naive
implementations copy and re-hash constantly. Purely functional languages have
fought this overhead for decades. vixen's answer is a two-state model that makes
the tax vanish where it matters:

- **Frozen** values live in the content-addressed store. They are immutable,
  hashed, deduplicated, shareable, memoizable — they have *identity*.
- **Molten** values are the working state *inside* a computation. They have no
  hash, no identity, and nothing outside the computation can see them. They are,
  quite literally, still hot.

The rule that makes this sound: **identity only exists at the boundary.** The
memo needs a value's hash only when the value crosses between computations — as
an argument, a result, a stored fact. Between those crossings, the machine is
free to do whatever is fastest, because *no observer exists* that could tell the
difference.

## Write functional, run imperative

In vix you write persistent-value code:

```
let trail = trail.push(fact)
```

Semantically, that's a new list. Physically, when the machine can see that the
old `trail` is not referenced by anyone else — a cheap runtime check, plus a
compile-time rule that a rebound receiver is a *move* — it mutates the buffer in
place. If an alias exists, it copies, and the semantics are identical either
way. There is a test mode that forces every operation down the copy path and
asserts the results and traces are indistinguishable; that equivalence is the
guarantee, enforced continuously.

The same idea extends to loops: tail-recursive calls compile to jumps, so an
accumulator threaded through a million iterations stays molten the whole way,
mutated in place, hashed exactly once — when the loop's answer finally freezes
at the boundary. (See [Loops](@/vix/loops.md).)

## Why not just "be fast" everywhere?

Because the boundary is where the value comes from. Freezing is what buys
memoization, dedup, early cutoff, distribution — the entire caching story.
Staying molten everywhere would make vix a fast ordinary language with no
memory; freezing everything eagerly makes it a correct slow one (this was the
measured state, once: 86% of the resolver's CPU was hashing). The two-state
model is the design that takes both: interiors run at imperative speed,
boundaries pay for exactly the identity the machine uses.

The numbers behind this page were measured on real workloads — a solver's trail
loop and a parser's stack loop — and the ladder from "7,300× slower than the
Rust reference" to "at the reuse ceiling" is documented in the development
records.
