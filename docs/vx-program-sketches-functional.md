# Vixen Program Sketches: Functional Variant

This note explores the active sketch direction for Vixen.

The point is not "replace all control flow with combinators". The point is:

- make collection transforms first-class
- let rule code read more like graph/data derivation
- avoid encoding obvious transforms as manual accumulation loops
- improve on Rust's error-handling surface instead of inheriting it blindly

## Working Assumptions

This variant now assumes:

- no `do/end`
- no `?`
- no Styx-style `@variant` notation
- no separate mutable-binding sigil
- no pipeline operator in v0
- closures exist in v0
- closures use `(x) => ...` syntax
- unary expression closures may use `$` shorthand
- block-bodied closures are allowed
- `match` is the branching surface
- short constructor/variant forms are inferred from type context
- methods are sugar over ordinary visible functions
- `place.method!(...)` is writeback sugar on places

That means code wants to look like:

```text
Node { ... }
emit::node { node }
graph.crates.filter((krate) => krate.crate_type == Bin)
metadata.insert!("crate_name", krate.crate_name)
```

## Error Model: Default Propagation, Explicit Local Handling

This branch flips Rust's default.

Inside a fallible rule:

- fallible calls produce their success value on the happy path
- errors propagate automatically by default
- `fail expr` exits the current `throws` boundary with an error
- `catch expr` suppresses propagation and yields a `Result`

So instead of:

```text
value = lookup(x)?
```

the happy path is just:

```text
value = lookup(x)
```

And the rare local-handling path is explicit:

```text
match catch lookup(x) {
  Ok(value) => ...
  Err(err) => ...
}
```

## Bindings, places, and writeback

Plain `=` always introduces a new binding:

```text
x = 1
x = x + 1
```

That is shadowing, not slot mutation.

Explicit place updates still exist:

```text
node.label = "compile"
items[i] = value
```

Writeback sugar uses `!` on the method name:

```text
items.push!(node)
metadata.insert!("crate_name", krate.crate_name)
```

with the intended reading:

```text
items = items.push(node)
metadata = metadata.insert("crate_name", krate.crate_name)
```

Current assumptions:

- the receiver must be a place expression
- the underlying call must return exactly the same type as the receiver place
- no separate mutable-binding syntax is required
- because bindings shadow, closures capture values, not shared mutable cells

## Sketch 1: Filter directly

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] {
  graph.crates.filter($.crate_type == Bin)
}
```

This is still the clearest argument for the branch.

Compared to the imperative form, it is simply closer to what the program means.

## Sketch 2: Map nodes, then emit them

```text
fn plan_crates(graph: CrateGraph) -> Unit {
  graph.crates
    .filter($.crate_type == Bin)
    .map(make_compile_node($))
    .each((node) => emit::node { node })
}

fn make_compile_node(krate: CrateNode) -> Node {
  Node {
    id: format::compile_node_id(krate.crate_name, krate.crate_type),
    kind: Action,
    label: `compile ${krate.crate_name}`,
    metadata: {
      crate_name: krate.crate_name,
      crate_type: "bin",
    },
  }
}
```

What this shows:

- the rule reads as selection, derivation, emission
- constructor calls and named-payload calls coexist naturally
- short variant forms reduce a lot of noise when the field type is obvious

## Sketch 3: Fallible transforms with block closures

```text
fn plan_edges(graph: CrateGraph) -> Unit throws InputError {
  graph.crates
    .flat_map((krate) => edges_for_crate(graph, krate))
    .each((edge) => emit::edge { edge })
}

fn edges_for_crate(graph: CrateGraph, krate: CrateNode) -> [Edge] throws InputError {
  compile_id = format::compile_node_id(krate.crate_name, krate.crate_type)

  krate.deps.map((dep) => {
    dep_krate = graph.lookup_crate(dep.crate_id)
    dep_id = format::compile_node_id(dep_krate.crate_name, dep_krate.crate_type)

    Edge {
      from: dep_id,
      to: compile_id,
      kind: Always,
      metadata: {},
    }
  })
}
```

What this shows:

- block closures are useful without dragging in a full Rust closure model
- no `?` is needed
- errors still propagate out of the enclosing `throws` boundary automatically

## Sketch 4: Match-only validation

```text
fn validate_registry_dependencies(graph: CrateGraph) -> Unit throws InputError {
  graph.crates.each((krate) => {
    match krate.source {
      Registry { name, version, checksum } => {
        exists = cargo::registry_package_exists(name, version)
        match {
          !exists => fail MissingRegistryCrate { name, version }
          _ => ()
        }

        valid = cargo::registry_checksum_matches(name, version, checksum)
        match {
          !valid => fail ChecksumMismatch { name, version }
          _ => ()
        }
      }
      Path { path } => ()
      _ => ()
    }
  })
}
```

What this shows:

- `match` can cover both scrutinee matching and boolean branching
- the language can stay readable without `if`
- `fail` reads better than `return Err(...)`

## Sketch 5: Local error interception stays explicit

```text
fn validate_registry_entry(name: String, version: String) -> Unit throws InputError {
  match catch cargo::registry_package_exists(name, version) {
    Ok(exists) => match {
      exists => ()
      _ => fail MissingRegistryCrate { name, version }
    }
    Err(err) => fail err
  }
}
```

This is the key inversion from Rust:

- propagation is implicit
- interception is explicit

## Sketch 6: Variant refinement plus `or`

```text
fn registry_source(krate: CrateNode) -> CrateSource::Registry throws InputError {
  krate.source as CrateSource::Registry or fail ExpectedRegistrySource {
    crate_name: krate.crate_name,
  }
}
```

What this shows:

- `Foo::Bar` being a real type is useful immediately
- `as` returning `Option<VariantType>` fits naturally
- `or` can act like `??`, not like falsy-coalescing

## Sketch 7: Plain maps and writeback methods

```text
fn node_metadata(krate: CrateNode) -> Map<String, String> {
  metadata = {}
  metadata.insert!("crate_name", krate.crate_name)
  metadata.insert!("crate_type", "bin")
  metadata
}
```

This is a good middle ground:

- plain maps stay lightweight
- direct field/index writes are still available when needed
- `insert!` makes the hidden rewrite explicit without needing a separate
  mutable-binding form

## Sketch 8: Breakable blocks and nonlocal `return`

```text
fn source_label(krate: CrateNode) -> String {
  @choose {
    match krate.source {
      Registry { name, version, checksum } => break choose `registry ${name}@${version}`
      Path { path } => break choose `path ${path}`
      _ => "unknown"
    }
  }
}

fn first_binary(graph: CrateGraph) -> Option<CrateNode> {
  graph.crates.each((krate) => {
    match krate.crate_type {
      Bin => return krate.some()
      _ => ()
    }
  })

  None
}
```

What this shows:

- `break` is the local escape hatch for blocks and closures
- `return` remains the nonlocal function escape hatch
- closures can participate in structured control flow without becoming tiny
  Rust functions

## Why This Branch Fits Vixen Well

Vixen rule logic is often about:

- selecting subsets
- deriving nodes from inputs
- expanding dependency edges
- transforming graph data
- surfacing typed diagnostics

That is naturally expressed as:

- `filter`
- `map`
- `flat_map`
- `each`
- `match`
- `fail`

not only as:

- `for`
- `if`
- manual accumulation locals

So the functional branch is not aesthetic for its own sake. It matches the
shape of the work.

## Current Read

This still feels stronger than the imperative-only branches.

The good version is not:

- everything becomes a pipeline

It is:

- use functional transforms where the problem is a transform
- use `match` and blocks where the problem is control flow
- make propagation the quiet path
- keep local rewrites explicit with `!`

The main remaining open questions are:

1. How strict should v0 exhaustiveness checking be?
2. How broad should method-call lookup be beyond visible names plus the prelude?
3. What exact punctuation policy should the formatter enforce?
4. What module/import surface should multi-file rule code use?
