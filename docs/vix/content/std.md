+++
title = "The standard library"
weight = 9

[extra]
updated = "2026-07-08"
+++

vix's standard library is small and grows by necessity — a function enters
when build logic actually needs it, written in vix itself wherever possible.
The bar for "must be a native primitive instead" is high and *measured*: the
machine makes naive vix implementations fast (see [Demand
semantics](/vix/demand/) and [Molten values](/vix/molten/)), so a naive
insertion sort in vix std competes with what you'd otherwise hand-optimize in
a host language.

## What ships today

**Versions** (`std/version.vix`) — full semantic versioning as a plain vix
struct: parsing (`1.2.3-alpha.1+build`), precedence-correct comparison
(prerelease ordering per the SemVer spec, build metadata ignored), the
comparison operators wired up. Verified differentially against the Rust
`semver` crate on hundreds of ordered pairs — the oracle-first style the
whole project prefers (see [Worked examples](/vix/examples/)). Version
*requirements* (`^1.2`, `~0.3`, ranges) pair with this for the resolver as a
`VersionSet` with `.contains()` matching.

**Strings** — the working set: `before`/`after` (split at first match),
`split`, `contains`, `starts`/`ends` checks, case mapping, `parse_int`,
numericness tests, concatenation with `+`, and ordering comparisons. UTF-8
throughout.

**Collections** — `[T]` arrays (`push`, `pop`, `set`, `len`, `map`, `filter`,
`collect`, `join`), `Map<K, V>` (`insert`, `get`, empty construction),
`Option<T>` (construction and pattern matching). See [Values and
types](/vix/values/) for the exact `pop` signature (it returns `(T, [T])`,
not `Option<T>`) and why maps collect in canonical sorted-key order. Sets are
currently spelled `Map<K, Bool>`; a real set type is on the ergonomics list,
as is a `sort` — the resolver has hand-rolled insertion sort twice, which is
exactly the kind of repeated need that promotes a function into std. See
[Honest gaps](/vix/gaps/).

## The direction

The library's trajectory follows one rule: if only build logic touches a
piece of behavior, it belongs in vix, not behind a Rust builtin. `Version`
handling already made this move — it *left* Rust entirely and became vix
source; the version-requirement algebra is following the same path; parsers
are staged to follow via [snark](/dev/constellation/snark/) grammars (see
[Document parsing](/vix/parsing/)). The standard library is expected to
absorb, over time, most of what today looks like a
[builtin](/vix/builtins/).
