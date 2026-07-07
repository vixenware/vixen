+++
title = "Demand semantics"
weight = 8

[extra]
updated = "2026-07-08"
+++

Every other page in this reference describes a *consequence*. This page
describes the *rule* they're consequences of: **nothing in vix forces
locally.** Not function application, not member access, not an operator, not
a conditional. Every expression becomes a node in a graph; evaluation
happens only when demand — entering at the edge, where a result is actually
selected — backpropagates through that graph and reaches it. "When does this
run" and "when is it demanded" are the same question, always, with no
exceptions the language lets you reach for.

This is why memoization, laziness, and exact incremental re-evaluation don't
need separate mechanisms in vix: they're all facets of the one thing —
demand-driven evaluation — rather than three independent features that
happen to be bolted onto the same runtime.

## Demand has a kind

There are two kinds of demand, and the distinction is the semantic heart of
the model:

- **Identity** — the whole value, forced, hashable, comparable. This is what
  a memo key needs, what a hash needs, what shipping a value across an
  execution boundary needs.
- **Path** — one path out of a tree-shaped value. Projecting `tree /
  p"Cargo.toml"` only ever asks "what's at this path" — it doesn't force the
  rest of the tree to exist, doesn't hash it, doesn't wait for a producer
  still writing other files to finish.

**Projection doesn't force; identity does.** A chain of `/` projections
composes into a single, still-unforced path demand; nothing about following
that chain costs a real computation until something downstream needs the
tree's *identity* — a hash, a comparison, an entry in a memo key, a mount
into a sandbox. For a non-tree value (an `Int`, a `String`, a struct),
`Path` demand just degenerates to `Identity` — there's no partial-projection
distinction to make for a scalar.

## Strictness points — the complete list

Everything that forces anything is one of these. Nothing else does:

1. **The edge.** Demanding an entry point's result forces it fully — this is
   where evaluation actually starts.
2. **Memo boundaries.** A function call demands every argument's identity,
   jointly, as one batch — the batch *is* the memo key's support. See
   [Functions and calls](/vix/functions/).
3. **The scrutinee of `match`.** Demanded as identity; the arms themselves
   stay lazy thunks, and only the taken arm's computation is ever demanded.
   See [Pattern matching](/vix/patterns/).
4. **Operators.** Strict in their operands, as identity — `a + b` demanded
   means both `a` and `b` are demanded, jointly; `a + b` never demanded means
   neither is.
5. **`&&` and `||` are conditionals, not strict operators.** They demand the
   left operand's identity, and the right operand *only if the left doesn't
   already decide the result*. Short-circuiting is demand-driven evaluation
   in miniature, not a special case bolted on top of it.
6. **Command-block splices** (an exec's argv, a mount). A whole-tree splice
   demands identity; an `Arg::Interpolation` subpath demands only that path.
   See [Exec nodes](/vix/exec/). A command block that's never demanded
   registers no run at all — not "ran but was ignored," genuinely never
   spawned.
7. **Structural iteration.** `.map(f).collect()` — `collect` demands the
   collection's identity, which demands each element's identity, jointly.
   Parallel fan-out is *discovered* this way, not annotated by the
   programmer: the machine sees a batch of independent memo boundaries and
   is free to run them concurrently.
8. **Observations** (`fetch`, capability acquisition). Strict when demanded —
   these produce values or pins, not partial trees, so there's no useful
   partial-demand shape for them.

Everything else — `let` bindings, function application itself (creates a
node, computes nothing), member access on a struct (projects the field,
doesn't force the rest of the struct) — is lazy by default.

## Bits are strict, identities are lazy

A value that might still be in flight (see `Pending<T>`/`Realized<T>` in
[Values and types](/vix/values/)) needs a precise rule for when "not
realized yet" has to resolve into an actual value. The rule: a demand is
inserted exactly where a possibly-pending value flows into a position that
needs raw bits — an arithmetic operand, a `match` tag read, a scalar written
into a memo-boundary argument. Composite values crossing a boundary stay
lazy (identities); scalars at a strictness point are made strict (bits). This
is deliberately not implemented as a single library function you call to
"force" a value — an early design made `Option.unwrap` play that role, and it
over-forced: passing an unwrapped value to a callee that never reads it would
still run the producer. `unwrap` stays a plain, possibly-lazy Option-shell
operation; the strictness lives at the typed boundary the compiler inserts,
not in something you call.

## Closures capture nodes, not values

Creating a closure captures the *nodes* of the bindings it closes over,
lazily — creating a closure computes nothing by itself. Using the closure's
identity (hashing it, comparing it, shipping it across an execution
boundary) forces its captures, exactly like every other identity-forcing
use forces the value it touches. There's no special case for closures in
the demand model; they're subject to the same Identity-forces,
projection-doesn't rule as everything else.

## Why this is the whole language, not an implementation detail

Every property described elsewhere in this reference falls out of this one
rule: [memoization and laziness](/vix/functions/) are what happens when a
memo boundary's demand is satisfied from an existing answer instead of a new
computation; [molten mutation](/vix/molten/) is sound because nothing except
a real demand boundary can observe a value's pre-mutation state; [tail
loops](/vix/loops/) exist because a self-tail-call is deliberately routed
around a fresh demand rather than through one. None of this is a runtime
optimization layered on top of an otherwise-ordinary language — the demand
graph *is* the language's semantics, and "ordinary-looking functional code"
is what writing against that semantics looks like from the outside.
