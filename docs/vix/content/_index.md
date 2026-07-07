+++
title = "The vix language"
+++

vix is the language vixen's build logic is written in — including vixen's own
understanding of what a Cargo build *is*. The dependency resolver, the
manifest parser, the unit-graph builder: all vix, running on the same demand
machine as everything else vixen does.

It exists because build logic has a shape that general-purpose languages
fight. Every build step should be memoized, every rebuild should be exact,
every read should be tracked — and normally you engineer all of that by hand,
crate by crate, forever. vix makes those properties ambient instead. You write
values, functions, pattern matching, ordinary-looking recursion. The machine
supplies memoization, incrementality, and content identity underneath, for
free. **The language only denotes; the machine supplies memo, identity, and
incrementality ambiently.**

This is the reference manual: what the language *is*, precisely, with real
excerpts from the programs that run it. For the machine's internals — how
this lowers to bytecode, how the store is laid out, how hashing works — see
[[dev:concepts/demand]] and the rest of the [developer site](/dev/).

## Who this is for

You're reading this because you're writing build logic for vixen (a resolver
fragment, a manifest accessor, a directive parser), or because you want to
know precisely what a language whose evaluation model *is* a build system's
semantics looks like. Either way: vix is small on purpose. It has structs,
enums, pattern matching, arrays, maps, options, and functions — and no
`while`, no classes, no exceptions, no async/await, and (deliberately) no
escape hatch to print-debug your way out of a wrong mental model. The
observability stack is the mitigation for that last one; see
[Diagnostics](/vix/diagnostics/).

## One real program, to anchor the rest

This is [`rodin/rodin.vix`](https://code.vixen.rs/vixen/facet/src/branch/main/rodin/rodin.vix)
— the CDCL-flavored dependency resolver vixen actually runs, not a toy:

```vix
enum Source {
    CratesIo,
    Registry(String),
    Git { url: String, rev: String },
    Path(String),
}

struct PackageId {
    source: Source,
    name: String,
    compat: Option<CompatClass>,
}

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

Nothing here declares a cache, a memo table, or a read-set. `compat_of` is an
ordinary function over an ordinary enum. It gets memoized, content-addressed,
and exactly-incremental anyway — because in vix, calling a function is
*demanding* a value, and demand is the machine's job, not yours. That single
idea is the throughline of every page below.

## The reference

Read top to bottom for the guided tour, or jump to what you need:

- **[Values and types](/vix/values/)** — scalars, `String`, `Array<T>`,
  `Map<K, V>`, `Option<T>`, tuples, structs and enums, `Version`, capability
  `Path`s, `Tree`, `Doc`, `Pending<T>`/`Realized<T>`.
- **[Functions and calls](/vix/functions/)** — a call is a demand: memoized,
  lazy, exactly re-evaluated on change.
- **[Pattern matching](/vix/patterns/)** — `match` over enums, structs, and
  literals; exhaustiveness; only the taken arm ever runs.
- **[Molten values and the consuming-move rule](/vix/molten/)** — why `let
  stack = stack.push(frame)` mutates in place, and the rebind idiom that makes
  it so.
- **[Loops](/vix/loops/)** — self-tail-calls compile to a jump, not a call:
  the one way to keep an accumulator hot across a million iterations.
- **[Exec nodes](/vix/exec/)** — `rustc!`, `build_script!`, and the lazy `Arg`
  argv model: "an `--extern` argument is a graph edge wearing an argv
  costume."
- **[Document parsing](/vix/parsing/)** — `toml()`, `json()`, `cfg(...)`: Doc
  values from snark grammars, coerced into typed values at the boundary.
- **[Demand semantics](/vix/demand/)** — the execution model underneath all
  of the above: "when does this run" *is* "when is it demanded."
- **[The standard library](/vix/std/)** — what ships today: versions,
  strings, collections.
- **[Builtins](/vix/builtins/)** — what's a permanent machine primitive vs.
  what's scheduled to become ordinary library code.
- **[Diagnostics](/vix/diagnostics/)** — observability instead of annotation:
  what the compiler tells you, and why.
- **[Worked examples](/vix/examples/)** — real fragments from the resolver,
  the manifest parser, and the unit graph, read closely.
- **[Honest gaps](/vix/gaps/)** — what isn't built yet, dated, from the
  project's own working notes.
