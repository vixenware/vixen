+++
title = "Document parsing"
weight = 7

[extra]
updated = "2026-07-08"
+++

Build logic reads a lot of semi-structured text — `Cargo.toml`, lockfiles,
`cfg(...)` predicates, JSON index files — and vix's answer is: parse it into
a `Doc`, navigate it dynamically, coerce into typed values at an explicit
boundary. See [Values and types](/vix/values/) for what `Doc` is and isn't.

## `toml()` and `json()`

```vix
fn manifest_doc(manifest: Tree) -> Doc {
    toml(manifest / p"Cargo.toml")
}
```

Calling `toml(path)` or `json(path)` on a projected file returns a `Doc` —
schema-less, structured, navigated with `.get(key)`. Coercing a field out of
a `Doc` into a typed value is always an explicit, checkable step:

```vix
fn doc_string(doc: Doc, key: String) -> String {
    doc.get(key).unwrap()
}

fn doc_bool_or(doc: Doc, key: String, fallback: Bool) -> Bool {
    match doc.get(key) {
        Some(value) => doc_as_bool(value),
        None => fallback,
    }
}
```
— `playgrounds/snark/src/bundled/vix/samples/cargo_manifest.vix`. The typed
table has caught real bugs this way: a `Doc::Array` was, at one point,
silently passing type-checking as an `Array<String>` before the coercion
step was tightened to actually verify element shape rather than trust the
call site.

## `cfg(...)` — a parsed grammar, not a substring match

`cfg(target_os = "linux")`-style predicates used to be evaluated the way
every ad-hoc cfg-handling code does it: substring matching over the raw
string. That's gone. `cfg(...)` is now a real [snark](/dev/constellation/snark/)
grammar — declarative, tree-sitter-shaped — that parses the predicate into
structured data carried on dependency edges, and a small pure-vix evaluator
walks that structure against the facts a target actually reports (`rustc
--print cfg --target <triple>`, demanded like any other observation — see
[Demand semantics](/vix/demand/)). Cross-target behavior is checked against
real `rustc` output as the oracle, the same discipline the resolver uses
against real `cargo`.

This is the shape every remaining parser is headed toward: a declarative
grammar produces structured data; a small amount of ordinary vix walks it.
Nothing about the *call site* changes when a parser makes this move — you
still write `toml(path)` or `cfg("...")` — only what's behind the call
changes, from a Rust builtin to inspectable, incremental vix.

## What's a builtin today, and what's staged to stop being one

- **JSON** — the grammar already exists; JSON is the first parser to make
  the move from builtin to snark-backed library code.
- **TOML** — staged to follow JSON's path.
- **`cfg(...)`** — already migrated, as above: the first primitive parser to
  fully complete the trip from Rust builtin to snark grammar + pure vix
  evaluator.
- **Archive extraction (`crate_archive`), ELF format probes** — still
  machine-provided today. ELF in particular is a harder case: parsing an ELF
  file is data-dependent pointer-chasing (offsets that depend on other
  offsets you've already read), which is Kaitai-shaped, not
  tree-sitter-shaped — a fundamentally different declarative parsing
  problem than a context-free grammar. The likely direction is a registered
  capability rather than a snark grammar, at least for now.

See [Builtins](/vix/builtins/) for the full three-way split (effects,
parsers, accessors) this fits into, and [Honest gaps](/vix/gaps/) for what's
still rough around `Doc` traversal (optional projection, key enumeration
over dependency tables) as of this writing.
