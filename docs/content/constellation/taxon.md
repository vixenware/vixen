+++
title = "taxon — types and identity"
+++

taxon is the smallest piece of the constellation and the one everything else
stands on: **a shared vocabulary for what types are, and a content-derived
identity for them.**

The vocabulary is deliberately boring — structs with named fields, enums with
variants, tuples, lists, sets, maps, options, primitives. No layout, no
language-specific detail. It exists because without it, every layer invents its
own: the reflection layer has one notion of "struct," the wire format another,
the language a third, and the build machine glues them together with strings.
(That was the actual archaeology: the machine once dispatched on
`schema.starts_with("Map<")`. The disease is always strings.)

The identity is the interesting half. A type's **SchemaId** is a hash of its
*canonical structure* — fields, names, shapes, recursively, with cycles handled
by construction. Two properties follow:

- **No coordination.** Any process, any language, any day: hash the same
  structure, get the same identity. Registries synchronize by arithmetic.
- **Structural honesty.** Rename a type but keep its shape: same identity.
  Change a field's type: new identity, and everything that depends on the old
  shape knows it.

This is what makes the rest of the system's promises composable. A value in the
content store, a message on the wire, a Rust struct in a running process, and a
vix value in the machine can all be *the same type* in a checkable sense — which
is what memoizing across process boundaries, validating stored data against
current code, and generating typed clients in other languages all quietly
require.

## Relationship to the neighbors

[facet](@/constellation/facet.md) describes Rust types; a facet shape *projects*
into a taxon type (losing Rust-specific detail like smart-pointer identity —
`Vec<T>` and `SmallVec<T>` are both just "list of T" structurally, which is
either the point or a caveat depending on your question).
[phon](@/constellation/phon.md) puts taxon-identified values on the wire.
[weavy](@/constellation/weavy.md) gives them concrete memory layouts — many
layouts per abstract type, which is the whole trick: *identity is structural,
layout is a realization.* The machine's content hashes and the store's keys
bottom out in taxon ids.

It is a tiny crate — no runtime dependencies beyond the hash function — and it
is intentionally the most stable thing in the stack: everything depends on it,
so it moves last.
