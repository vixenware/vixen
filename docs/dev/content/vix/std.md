+++
title = "The standard library"
weight = 5
+++

vix's standard library is small and grows by necessity — a function enters when
build logic needs it, written in vix itself wherever possible (the machine
makes naive implementations fast, so the bar for "must be a native primitive"
is high and *measured*).

What ships today:

**Versions** (`std/version.vix`) — full semantic versioning as a plain vix
struct: parsing (`1.2.3-alpha.1+build`), precedence-correct comparison
(prerelease ordering per the SemVer spec, build metadata ignored), the
comparison operators wired. Verified differentially against the Rust `semver`
crate on hundreds of ordered pairs — the oracle style the whole project
prefers. Version *requirements* (`^1.2`, `~0.3`, ranges) pair with this for
the resolver.

**Strings** — the working set: `before`/`after` (split at first match),
`split`, `contains`, `starts/ends` checks, case mapping, `parse_int`,
numericness tests, concatenation with `+`, and ordering comparisons. UTF-8
throughout.

**Collections** — `[T]` arrays (`push`, `pop`, `set`, `len`, `map`, `filter`,
`collect`, `join`), `Map<K, V>` (`insert`, `get`, empty construction),
`Option<T>` (construction and pattern matching). Sets are currently spelled
`Map<K, Bool>`; a real set type is on the ergonomics list, as is a `sort` —
the resolver hand-rolled insertion sort twice, which is the kind of signal
that promotes a function into std.

## The direction

The library's trajectory follows one rule from
[where types live](/constellation/facet/): if only build logic touches it,
it belongs in vix. Version handling *left* Rust and became vix source; the
version-requirement algebra is following; parsers are staged to follow via
[snark](/constellation/snark/). The standard library is expected to
absorb, over time, most of what today looks like
[builtins](/vix/builtins/).
