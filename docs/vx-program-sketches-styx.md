# Vixen Program Sketches: Styx-Heavy Variant

This document is intentionally not conservative.

It is a pressure test for what Vixen might look like if it took far more
inspiration from Styx than the current baseline does.

The point is not "this is the new proposal". The point is:

- stop assuming Vixen has to be small Rust
- push harder on punctuation removal
- use tagged values more aggressively
- see whether real rule logic becomes more readable or more magical

If the baseline sketches in
[vx-program-sketches.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches.md)
ask "what if Vixen stayed fairly Rust-like?",
this note asks "what if Vixen became much more declarative and Styx-shaped?"

## Working Assumptions for This Variant

This variant assumes all of the following, even if the baseline does not:

- statements are newline-separated by default
- semicolons disappear
- multiline records and calls do not need commas
- object/record fields use `key value`, not `key: value`
- tags carry a lot of sum-type weight
- bare scalars are preferred when quoting is unnecessary
- effectful statement forms can be written command-style
- typed context may remove some constructor noise

This is much closer to "code with configuration-language ergonomics" than to
"Rust with fewer features".

## Sketch 1: Plan compile nodes for binary crates

```text
fn plan_crates graph: CrateGraph -> Unit {
    for krate in graph.crates {
        if krate.crate_type == @bin {
            let node_id format::compile_node_id(
                krate.crate_name
                krate.crate_type
            )

            emit::node @Node{
                id node_id
                kind @action
                label `compile ${krate.crate_name}`
                metadata {
                    crate_name krate.crate_name
                    crate_type bin
                }
            }
        }
    }
}
```

What changes here:

- record construction is much lighter
- enum-like values become tags (`@bin`, `@action`)
- metadata construction no longer needs chained `insert`
- the statement `emit::node ...` reads more like a declarative emission than a
  normal function call

What feels good:

- this is materially less noisy than the baseline version
- the metadata block is much better than `{}.insert(...).insert(...)`

What feels risky:

- `id node_id` assumes a lot of contextual typing
- `@Node{...}` as a constructor is elegant, but it is a meaningful break from
  Rust mental models

## Sketch 2: Plan compile edges from dependency closure

```text
fn plan_edges graph: CrateGraph -> Unit {
    for krate in graph.crates {
        let compile_id format::compile_node_id(
            krate.crate_name
            krate.crate_type
        )

        for dep in krate.deps {
            let dep_krate graph.lookup_crate(dep.crate_id)?
            let dep_id format::compile_node_id(
                dep_krate.crate_name
                dep_krate.crate_type
            )

            emit::edge @Edge{
                from dep_id
                to compile_id
                kind @always
                metadata {}
            }
        }
    }
}
```

What changes here:

- command-style emission still reads cleanly in nested loops
- `@always` is a nicer surface than `EdgeKind::Always`
- newline-separated arguments make multiline calls less punctuation-heavy

What feels good:

- the overall shape is very readable
- domain variants as tags are more natural than fully-qualified enum names

What feels risky:

- it depends on the parser handling newline-driven calls and records cleanly
- if Vixen keeps `?`, the language starts mixing very different aesthetic
  traditions

## Sketch 3: Validate manifest-derived input

This variant leans harder into tags as the primary enum surface.

```text
fn validate_registry_dependencies graph: CrateGraph -> InputCheck {
    for krate in graph.crates {
        match krate.source {
            @registry{name version checksum} {
                if !cargo::registry_package_exists(name version)? {
                    return @missing_registry_crate{
                        name name
                        version version
                    }
                }

                if !cargo::registry_checksum_matches(name version checksum)? {
                    return @checksum_mismatch{
                        name name
                        version version
                    }
                }
            }

            @path{} {}
        }
    }

    @ok
}
```

What changes here:

- match patterns become much more compact
- result-like values can be plain tags instead of `Result<_, _>` ceremony
- domain errors read like structured tagged data, not enum boilerplate

What feels good:

- this is probably the strongest case for Styx influence
- tagged variants are excellent for build/domain diagnostics

What feels risky:

- this drifts away from Rust-shaped error handling significantly
- if Vixen also wants `Option`/`Result`, there may be too many parallel idioms

## Sketch 4: Build a filtered list without closures

This variant assumes statement-position collection updates can be command-like.

```text
fn binary_crates graph: CrateGraph -> [CrateNode] {
    let mut out ()

    for krate in graph.crates {
        if krate.crate_type == @bin {
            push out krate
        }
    }

    out
}
```

Alternative, if command forms are too magical:

```text
fn binary_crates graph: CrateGraph -> [CrateNode] {
    let mut out ()

    for krate in graph.crates {
        if krate.crate_type == @bin {
            out = list::push(out krate)
        }
    }

    out
}
```

What changes here:

- list literals become sequence-like `()`
- mutation can be expressed without methods
- collection-building code becomes more statement-oriented

What feels good:

- `push out krate` is much nicer than `out = out.push(krate)` if it can be made
  principled

What feels risky:

- this introduces special statement forms
- `()` as the empty list is very Styx, but it may be too big a break from
  ordinary code expectations

## Sketch 5: Construct a compile node directly

```text
fn make_compile_node krate: CrateNode -> Node {
    @Node{
        id format::compile_node_id(
            krate.crate_name
            krate.crate_type
        )
        kind @action
        label `compile ${krate.crate_name}`
        metadata {
            crate_name krate.crate_name
            crate_type bin
        }
    }
}
```

What changes here:

- a typed record literal can stand alone as the tail expression
- field syntax becomes very close to Styx object entries
- contextual construction removes a lot of ceremony

What feels good:

- this is drastically better than map-builder chains
- the overall node shape is easy to scan

What feels risky:

- without commas and colons, formatting discipline becomes much more important
- type-directed interpretation is doing a lot of work off-screen

## Sketch 6: Multi-file rule module, very Styx-flavored

The baseline has mostly deferred modules/imports. A Styx-heavy design might go
more document-shaped instead.

```text
module cargo

rule plan_crates graph: CrateGraph -> Unit {
    use rules::format
    use rules::emit

    for krate in graph.crates {
        if krate.crate_type == @bin {
            emit::node @Node{
                id format::compile_node_id(
                    krate.crate_name
                    krate.crate_type
                )
                kind @action
                label `compile ${krate.crate_name}`
            }
        }
    }
}
```

This is not a proposal for `use`. It is here to show that once the surface gets
lighter, module syntax can probably afford to be lighter too.

## What This Version Seems To Want

If Vixen really went this way, it would probably want:

- newline-oriented statement syntax
- tagged values as the main enum surface
- record literals that look much closer to Styx objects
- command-style effect statements for `emit`, `push`, and similar operations
- fewer mandatory delimiters in multiline code
- stronger formatter authority

That is no longer "Rust but smaller". It is more like:

- a typed rule language
- with code control flow
- and a very configuration-language-friendly surface

## What Seems Genuinely Better

- tagged domain variants are much nicer than verbose enum qualification
- `key value` record fields are much nicer than repeated colons and commas in
  multiline rule data
- metadata-heavy code becomes dramatically better
- build-rule code starts to look like structured declarations, which is probably
  correct

## What Seems Dangerous

- too much context-sensitive interpretation could make code feel magical
- mixing command forms, expression forms, and tag forms could create a second
  kind of complexity
- the more punctuation we remove, the more the formatter becomes part of the
  language design
- if expressions get any richer, this style could become muddy quickly

## Current Read

I do not think Vixen should blindly become this language.

But I do think this pressure test reveals three things:

1. Tagged enums are probably more promising than Rust-style enum spelling for
   rule code.
2. Styx-like record fields (`key value`) are genuinely attractive in multiline
   typed construction.
3. The ugliness in the current sketches is less about control flow and more
   about constructor punctuation and collection-update ceremony.

That means the best next comparison is probably not:

- Rust-like Vixen
- versus fully Styx-like Vixen

It is:

- Rust-like control flow and declarations
- plus Styx-like tagged variants and record literals

That hybrid may be the real sweet spot.
