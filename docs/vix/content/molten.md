+++
title = "Molten values and the consuming-move rule"
weight = 4

[extra]
updated = "2026-07-08"
+++

Values are **molten** — hot, identity-free, mutable when nothing else can
observe the difference — while they're being built or updated inside a
computation, and **freeze** into content-addressed, immutable store entries
only when they cross a demand boundary (a return, a capture, a hash). This
page is the practical contract: the idiom that gets you in-place mutation,
when copying happens instead, and the guarantee that neither choice is ever
visible in your program's result.

## The idiom that matters

```vix
let stack = stack.push(frame)
```

Rebinding a name with an update of itself is the canonical shape. The
compiler treats the receiver (`stack`, on the right) as **consumed**: nothing
in the rest of the function can read the old `stack` after this line, because
the name `stack` now means the new value. Because nothing can prove otherwise,
no reference count needs bumping just to read it, and the runtime check "is
this buffer uniquely mine?" (`refs == 1`) passes — the push **mutates in
place**. In a [tail loop](/vix/loops/), this composes into the case that
matters most: an accumulator threaded through a hundred thousand iterations,
one buffer, zero copies, one intern at the very end.

## When copying happens instead

```vix
let a = [1, 2];
let b = a;              // an alias now exists
let a = a.push(3);      // copies — b must still see [1, 2]
```

Aliasing is always semantically fine — vix is purely functional, so `b`
seeing `[1, 2]` after `a` becomes `[1, 2, 3]` is simply correct — it just
costs a copy the next time the aliased value is updated. The machine decides
mutate-vs-copy per operation, at runtime, from a reference count on the
molten entry: five host operations (`push`, `pop`, `set` on arrays, `insert`
on maps, and struct field update) each check `refs == 1` before deciding
whether to mutate the existing molten entry or clone-then-allocate a fresh
one. You never annotate anything, and you can never corrupt an alias — the
two code paths are proven observationally identical, not just assumed to be:
a standing test runs representative programs (chained array pushes and pops,
chained map inserts, a struct spread-update) twice each, once with reuse
enabled and once with an environment switch (`VIX_FORCE_MOLTEN_COPY`) that
forces every mutating op onto the copy path regardless of refcount, and
asserts the numeric result, the *entire* event trace, and the exported value
bytes are identical between the two runs. Reuse changes how a value gets
built, never what it is or what got observed while building it.

## How the rebind is detected

The consuming-move rule that makes `let x = x.push(v)` hit the fast path has
three parts, and they're worth naming because they explain exactly which
shapes get the optimization and which don't:

1. **Detecting the shape.** A `let` binding whose right-hand side is an
   aggregate-update method (`push`, `pop`, `set`, `insert`, or a `.pop().0`
   projection off one) called on a bare identifier matching the name being
   rebound is tagged as a *consuming rebind*.
2. **Skipping the alias-marking call.** Normally, reading a molten-typed slot
   emits a host call that bumps its refcount (marking it "some other name
   might still see this") before using it. A tagged consuming rebind skips
   that call entirely — it copies the frame *word* (just the handle integer,
   not the value), so the refcount never moves and the aggregate-update
   method sees `refs == 1`.
3. **The escape hatch: pending aliases.** If some *other*, not-yet-evaluated
   binding still textually mentions the name being rebound (`let b = a;`
   sitting above `let a = a.push(2);`, where `b` hasn't been forced yet), the
   consuming-move optimization is refused and the ordinary alias-marking path
   runs instead — `b` still sees the pre-push array, correctly, at the cost
   of a copy.

This is deliberately a **syntactic**, conservative check — not a full
liveness analysis — but it's exactly what the flagship accumulator idiom
needs, and it's precise enough to matter: the resolver's conflict-learning
trail loop went from 1.6–3.7ms to 70.8µs for 2048 pushes once this landed,
landing within 6% of the theoretical reuse ceiling.

## The current honest edges

- **A value read more than once stays copy-mode for the rest of that
  execution, even after every other reference to it has genuinely died.**
  Refcounts here are monotone: there is no decrement, only the one increment
  (the alias-marking call) and the `== 1` check. Reclaiming uniqueness after
  aliases provably go out of scope — a last-use drop analysis — is designed
  and queued (tracked as `MOLTEN_DROP` in the project's own notes); it's
  explicitly scoped as a soundness-critical change (an early drop is a
  correctness bug, not just a missed optimization), gated on the same
  differential-oracle discipline as reuse itself. Strictly-linear code (the
  rebind idiom, used consistently) is unaffected by this gap — it never
  creates the extra alias in the first place.
- **Crossing a demand boundary always freezes.** Passing your accumulator to
  another function hands the machine a value that needs identity to cross —
  that's a hash, and hashing ends molten-ness for that value. Keep hot
  interiors inside one function; that's what [tail loops](/vix/loops/) are
  for.
- **Performance is observable, not guessable.** When reuse declines — an
  alias forced a copy inside what you meant to be a hot loop — that shows up
  as a diagnostic you can ask for, not a mystery you profile for an
  afternoon. See [Diagnostics](/vix/diagnostics/).
