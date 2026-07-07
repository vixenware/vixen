+++
title = "rodin — the resolver"
weight = 5
+++

rodin is vixen's dependency resolver: given manifests and a package index,
decide which version of everything to build — the job Cargo's resolver does,
solved Cargo-compatibly, **written in vix**.

That last clause is the point. Version resolution is a constraint-solving
search: propagate requirements, pick candidates, hit conflicts, backtrack,
learn. It is exactly the kind of computation build systems usually bury in
native code where nothing about it is incremental or inspectable. In vix, the
solver's structure is visible to the machine:

- The **solve is a demand** — memoized by the content of its inputs. Re-resolve
  an unchanged workspace: cache hit, microseconds.
- The **interior stays molten** — the propagation loop threads its state
  through tail-recursive narrowing steps that run in place, at machine speed.
  (This workload drove the language's performance work; the measurements are
  in the development records.)
- **Learned facts are values** — conflict clauses survive as first-class,
  content-addressed data, which is what makes *warm* re-resolution possible:
  when one manifest line changes, prior learning still applies.

## Cargo compatibility, measured not claimed

rodin's correctness bar is differential: fixtures are materialized as real
Cargo workspaces, resolved by rodin *and* by real `cargo`, and the selections
are compared. The current honest score is on the [Status](@/status/_index.md)
page — the remaining gaps are named (weak-feature unification is the known
frontier), and the deliberately-unsupported tail of Cargo behaviors will be
documented as policy, not discovered as surprise.

## The name

Rodin sculpted; the resolver carves a consistent shape out of a block of
possibilities. Also, the reference implementation this one was distilled from
kept getting mistaken for the product — so it was retired, and its knowledge
lives on as specification documents. The vix implementation is the only one.
There is a lesson in that story about what code *is* for, and the
[development docs](@/developing/holes.md) tell it properly.
