+++
title = "Molten values in practice"
+++

The [concept page](@/concepts/molten.md) explains the model: values are molten
(hot, identity-free, mutable-when-unobservable) inside a computation and freeze
at the boundaries. This page is the practical contract.

## The idiom that matters

```
let stack = stack.push(frame)
```

Rebinding the same name with an update of itself is the canonical shape. The
compiler treats the receiver as *consumed* (nothing can use the old `stack`
afterward — the name now means the new one), so no reference count is spent
reading it, and the runtime check "is this buffer uniquely mine?" passes: the
push **mutates in place**. In a [tail loop](@/vix/loops.md), this composes
into the important case — an accumulator threaded through a million
iterations, one buffer, zero copies.

## When copying happens instead

```
let a = [1, 2];
let b = a;              // an alias now exists
let a = a.push(3);      // copies — b must still see [1, 2]
```

Aliasing is *fine* — it's functional semantics, always correct — it just costs
a copy when you then update a shared value. The machine decides mutate-vs-copy
per operation, at runtime, from reference counts; you never annotate anything,
and you can never corrupt an alias. The guarantee is enforced brutally in
vixen's own tests: every program can run in a force-copy mode, and results,
traces, and stores must be *indistinguishable* from the in-place mode.

## The current honest edges

- **A value read more than once stays copy-mode for now** even after the extra
  references die — reclaiming uniqueness (a drop analysis) is designed and
  queued. Strictly-linear code (the rebind idiom) is unaffected.
- **Crossing a demand freezes.** Passing your accumulator to another function
  hands the machine a value that needs identity — that's a hash. Keep hot
  interiors inside one function; that's what tail loops are for.
- **Performance is observable, not guessable.** When reuse declines (aliasing
  forced a copy in a loop), that's surfaced as a diagnostic you can ask for —
  the philosophy is that you should never have to *wonder* why a loop is slow.
