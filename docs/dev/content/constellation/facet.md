+++
title = "facet — Rust reflection"
weight = 6
+++

facet is how Rust types enter the constellation. Deriving `Facet` on a type
produces a complete structural description — fields, offsets, enum variants and
their discriminants, plus the function pointers needed to construct and
manipulate values — available at runtime, with no code generation step and no
schema files.

In the wider Rust ecosystem, facet is a reflection-and-serialization stack (the
role serde plays elsewhere, approached differently: one derive, many consumers —
JSON, TOML, pretty-printing, diffing, all reading the same shape description).
Inside the constellation it plays a specific, load-bearing role:

**Rust is the one layout authority we don't control.** The Rust compiler
decides where a struct's fields go, and it makes no promises. So for values
that cross between Rust code and the machine — configuration, wire messages,
build-system data structures — facet *discovers* the layout the compiler chose,
and [weavy](/constellation/weavy/) descriptors are built from that
discovery. The rule of the house: a type lives in Rust when Rust code
manipulates it; it lives in vix when only build logic touches it. facet is
what makes the first half of that rule workable — and the boundary is a
deliberate design decision, documented in the language section.

**Shapes project into [taxon](/constellation/taxon/).** A facet shape
carries Rust-specific detail (which smart pointer, which repr, declaration
identity). Its taxon projection keeps only structure — which is what gives a
Rust type the same content-derived identity as the equivalent vix type or wire
schema. One type, three views: Rust's (facet), abstract (taxon), laid-out
(weavy).

## Scope, honestly

facet reflects types that opt in — `#[derive(Facet)]` with a defined
representation. It does not and will not reflect arbitrary foreign crates'
types; the answer there is wrapper types, not heroics. That constraint is
inherited by everything downstream and it's the right trade: reflection you
can trust over reflection that covers everything.
