+++
title = "Pattern matching"
weight = 3
type = "reference"
updated = "2026-07-08"
+++

`match` is how vix branches — there is no `if`/`else` chain for anything
beyond a boolean condition, and no `switch`-with-fallthrough. Every arm is
checked against the scrutinee's declared shape, exhaustively, before the
machine ever runs the program: the checker verifies pattern coverage against
known shapes the same way it verifies field access against declarations.

## Matching booleans, enums, and payloads

The simplest shape — matching a boolean condition as if it were a two-variant
enum — is idiomatic vix, not a workaround:

```vix
fn compat_of(v: Version) -> CompatClass {
    match v.major != 0 {
        true => CompatClass::Major(v.major),
        false => match v.minor != 0 {
            true => CompatClass::Minor(v.minor),
            false => CompatClass::Patch(v.patch),
        },
    }
}
```
— `rodin/rodin.vix`. Nested `match` on booleans reads like a decision tree
because that's what it is; there's no separate `if` syntax to reach for
first.

Matching real enum variants, including struct-shaped and tuple-shaped
payloads, from the resolver's search loop:

```vix
fn search(index: Index, state: State, target: String) -> SearchResult {
    match propagate(index, state, target) {
        SearchResult::Failed(failed) => SearchResult::Failed(failed),
        SearchResult::Solved(propagated) => match all_decided(index, propagated) {
            true => SearchResult::Solved(propagated),
            false => match next_undecided(propagated, index.packages) {
                PackageChoice::NoPackage => SearchResult::Solved(propagated),
                PackageChoice::Picked(pkg) => decide_package(index, propagated, target, pkg),
            },
        },
    }
}
```
— `rodin/rodin.vix:1305-1340`. Each arm binds the payload it matches
(`failed`, `propagated`, `pkg`) as an ordinary local for the rest of that
arm's expression.

## `Option<T>` is matched, not null-checked

```vix
fn doc_bool_or(doc: Doc, key: String, fallback: Bool) -> Bool {
    match doc.get(key) {
        Some(value) => doc_as_bool(value),
        None => fallback,
    }
}
```
— `playgrounds/snark/src/bundled/vix/samples/cargo_manifest.vix`. There is no
null, no undefined, no implicit coercion of absence to falsy — `Option<T>` is
an ordinary two-variant enum, and `Some`/`None` are matched like any other
variant.

## Only the taken arm is ever demanded

This is not just an efficiency note, it's a checked structural property of
the language (see [Demand semantics](/vix/demand/) for the full strictness
model): the scrutinee is demanded — matching has to know which arm applies —
but the arms themselves are lazy thunks, and only the one that's selected
gets its computation demanded. An expensive computation sitting in an
untaken match arm never runs, never appears in the demand trace, and (if it
would have called an `exec!` primitive) never registers a run. The
differential test corpus asserts this directly by trace absence, not by
inference from the result being correct.

## Exhaustiveness and the checker

The checker rejects a `match` that doesn't cover every variant of the
scrutinee's declared type — there's no default/wildcard-required escape hatch
for enums the way there might be in a dynamically-typed language, because the
checker already knows every variant that can exist. This is part of the same
discipline that makes field access and function calls checkable before the
machine runs: see [Diagnostics](/vix/diagnostics/).

## What doesn't fit a pattern

`Doc` — the dynamic, schema-less value you get back from a parser — is
deliberately **not** pattern-matched. It's navigated with `.get(key)` and
coerced into a typed value explicitly (`doc_as_bool`, `doc_string`, and
similar accessors), because a `Doc`'s shape isn't known to the checker ahead
of time. See [Values and types](/vix/values/) and
[Document parsing](/vix/parsing/).
