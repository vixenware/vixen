+++
title = "Honest gaps"
weight = 13
type = "gaps"
updated = "2026-07-08"
+++

This page exists on purpose. vix is being built by porting real capability
out of a Rust reference (`rodin-core` and friends) into the language, one
piece at a time, validated against `cargo` as the oracle at every step — see
[[dev:developing/method]]. That means there's always an honest, dated list of
"not yet," logged by the missions that hit the wall, not smoothed over. Where
this page and a page elsewhere in this reference disagree, this page is the
more current one — the rest of the reference describes the design; this page
tracks where the implementation is still catching up to it, or where the
design itself is still open.

## No general loop syntax; mutual tail recursion isn't covered

[Loops](/vix/loops/) only lowers a **literal self**-tail-call to a backward
jump — a function calling itself, by exact identity, in tail position.
Mutual tail recursion (two or more functions jumping to each other as a
state machine) is designed and queued, not shipped. This isn't hypothetical:
the resolver's own feature-activation loop is exactly this shape today (see
[Worked examples](/vix/examples/)) and currently pays a full memo-boundary
INVOKE per step rather than running as a jump.

## No `MOLTEN_DROP` yet (molten reuse is soundness-only)

Refcounts on molten values only ever increment; there's no decrement path.
A value read more than once stays on the copy path for the rest of that
execution, even once every other reference to it has genuinely gone out of
scope. The planned unlock (a last-use drop analysis) is designed but not
started, and is explicitly scoped as safety-critical — an early drop would
be a correctness bug, not just a missed optimization. See [Molten
values](/vix/molten/).

## No `sort` primitive

The resolver has hand-rolled insertion sort more than once rather than reach
for a standard one. This is exactly the kind of repeated need that promotes
a function into [the standard library](/vix/std/) — it just hasn't happened
yet.

## Aggregates inside containers are still awkward

A `Map` or `Array` whose *elements* are themselves structs or enums hits
rough edges around the `Pending<T>`/`Realized<T>` machinery (see [Values and
types](/vix/values/)) — real code has worked around this with flat
integer-keyed row maps (an `Int` id standing in for the aggregate, with the
actual fields living in parallel maps) rather than a container of the
aggregate directly. Real typed collections landed — `Array<T>`, `Map<K,V>`,
`Option<T>` — but aggregates-as-elements is the part still catching up.

## `[String]` returns are unstable; appended fixture code can't call imported std helpers

Two ergonomic rough edges logged by the missions that hit them: a function
returning `[String]` has shown instability worth tracking down before
leaning on it further, and code appended into a fixture/test corpus can't yet
call helpers imported from the standard library the way ordinary module code
can.

## No block expressions in match arms

A `match` arm's body is a single expression — you can't drop a multi-statement
block (`let` followed by a tail expression) directly into an arm the way you
can as a function body. Where this matters today, the workaround is a named
helper function per arm (visible throughout the [worked
examples](/vix/examples/) — `_tuple` and `_after_pop` helper functions exist
partly because of this).

## `Doc` traversal ergonomics

Optional projection into a `Doc` (asking for a key that might not be there
without failing lowering when it isn't) and key enumeration over
dependency-table-shaped `Doc`s both have rough edges surfaced by manifest
ingestion work. See [Document parsing](/vix/parsing/).

## What has *landed* since earlier notes about this language

Some capabilities that used to be open questions are resolved as of this
writing, and are documented as shipped in the rest of this reference rather
than listed here as gaps — worth naming explicitly since earlier working
notes about vix predate them:

- **`Version` ordering** — full semver precedence (including prerelease
  ordering per the SemVer spec) is implemented and verified differentially
  against the Rust `semver` crate. See [The standard library](/vix/std/).
- **Capability `Path`s** — join-only, no free `String`-to-`Path` cast; an
  earlier draft allowed the cast and it was caught and reworked before
  landing. See [Values and types](/vix/values/).
- **Typed exec argv** (`Arg::{Str, Path, Interpolation}`) and single-file
  `Tree.text()` projection — both shipped. See [Exec nodes](/vix/exec/).
- **`cfg(...)` as a snark grammar** with a pure-vix evaluator, replacing a
  substring-matching scaffold — shipped. See [Document parsing](/vix/parsing/).

## Where this list comes from

This page distills the project's own dated working notes (`NEXT.md`'s
"vix language gaps" mission log and the surrounding fold history) rather
than a static design document — which is also why it's dated and expected to
shrink over time as missions clear items off it. If you're extending vix and
hit one of these, you're not missing something; log it the same way.
