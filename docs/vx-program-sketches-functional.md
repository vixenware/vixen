# Vixen Program Sketches: Functional Variant

This note explores a more functional direction for Vixen.

The point is not "replace all control flow with combinators". The point is:

- make collection transforms first-class
- let rule code read more like graph/data derivation
- avoid encoding obvious transforms as manual accumulation loops
- improve on Rust's error-handling surface instead of inheriting it blindly

This branch now assumes:

- no `do/end`
- no `:=`
- no `?`
- no Styx-style `@variant` notation

## Working Assumptions

This variant assumes:

- ordinary blocks use `{ ... }`
- closures exist in v0
- v0 closures are expression closures only: `|x| expr`
- collection APIs like `map`, `filter`, `flat_map`, `fold`, and `each` are
  first-class
- recursion is still out
- higher-order collection operations are still bounded by input collections
- enum variants use Rust-style spelling
- named payloads use a space before `{ ... }`

That means:

```text
Node { ... }
emit::node { ... }
CrateType::Bin
NodeKind::Action
```

The constructor/call rule is now:

- `callee(...)` for positional payload
- `callee { ... }` for named payload

where `callee` can be:

- a function
- a method
- a type/constructor

## Error Model: Default Propagation, Explicit Local Handling

This branch flips Rust's default.

Inside a fallible rule:

- fallible calls produce their success value on the happy path
- errors propagate automatically by default
- `fail expr` exits the current rule with an error
- `catch expr` suppresses propagation and yields the error-carrying value for
  local handling

So instead of:

```text
value = lookup(x)?
```

the default is just:

```text
value = lookup(x)
```

And if you actually want to inspect the error locally:

```text
result = catch lookup(x)
match result {
  Result::Ok(value) => ...
  Result::Err(err) => ...
}
```

This is still result-like under the hood. It just makes propagation the quiet
path and local interception the noisy one.

## Binding and Mutation Surface

Current bias for this branch:

```text
node_id = format::compile_node_id(name, ty)
nodes! = []
nodes.push!(node)
```

With the reading:

- `name = expr` introduces an immutable binding
- `name! = expr` introduces a mutable binding by shadowing earlier bindings
- `place.method!(args)` is sugar for rebinding the current mutable place

So:

```text
nodes! = []
nodes.push!(node)
nodes.push!(other)
```

behaves like repeated rebinding of the same mutable local, without needing
`let mut` or `nodes = nodes.push(...)`.

In v0, the receiver for `method!` should probably stay narrow:

- the receiver must be a place expression rooted in a mutable binding
- `nodes.push!(node)` is fine
- `graph.crates.push!(node)` is fine if `graph` is a mutable binding
- `make_graph().crates.push!(node)` should probably be invalid

That keeps the sugar explicit about what is being rebound.

This also means:

- bindings do not share mutable identity by default
- later shadowing does not retroactively change previously captured values
- closures see the value they captured, not a shared mutable cell

That is probably acceptable for transform-oriented rule code.
If Vixen ever genuinely needs shared mutable identity, it should probably use an
explicit cell/reference type rather than making it the default meaning of
bindings.

## Closure Surface

The smallest plausible closure surface is probably:

```text
|krate| krate.crate_type == CrateType::Bin
|krate| make_compile_node(krate)
|plan, node| plan.add_node(node)
```

This branch deliberately avoids multi-statement closure bodies in v0.

If a transform needs multiple steps, the current bias is:

- extract a named helper function
- or fall back to imperative control flow

That keeps closures simple while still unlocking the highest-value cases.

## Sketch 1: Filter directly

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] {
  graph.crates.filter(|krate| krate.crate_type == CrateType::Bin)
}
```

This is still the clearest argument for the whole branch.

Compared to the imperative form, this is simply closer to what the program
means.

## Sketch 2: Map nodes, then emit them

```text
fn plan_crates(graph: CrateGraph) -> Unit {
  graph.crates
    .filter(|krate| krate.crate_type == CrateType::Bin)
    .map(|krate| make_compile_node(krate))
    .each(|node| emit::node { node })
}

fn make_compile_node(krate: CrateNode) -> Node {
  Node {
    id: format::compile_node_id(krate.crate_name, krate.crate_type),
    kind: NodeKind::Action,
    label: `compile ${krate.crate_name}`,
    metadata: {
      "crate_name": krate.crate_name,
      "crate_type": "bin",
    },
  }
}
```

What this shows:

- the rule reads as selection, derivation, emission
- constructor calls and named-payload calls coexist naturally
- expression closures are enough if helper extraction is cheap

## Sketch 3: Auto-propagating fallible closures

```text
fn plan_edges(graph: CrateGraph) -> Unit throws InputError {
  graph.crates
    .flat_map(|krate| edges_for_crate(graph, krate))
    .each(|edge| emit::edge { edge })
}

fn edges_for_crate(graph: CrateGraph, krate: CrateNode) -> [Edge] throws InputError {
  compile_id = format::compile_node_id(krate.crate_name, krate.crate_type)

  krate.deps.map(|dep| edge_for_dep(graph, compile_id, dep))
}

fn edge_for_dep(graph: CrateGraph, compile_id: NodeId, dep: DepEdge) -> Edge throws InputError {
  dep_krate = graph.lookup_crate(dep.crate_id)
  dep_id = format::compile_node_id(dep_krate.crate_name, dep_krate.crate_type)

  Edge {
    from: dep_id,
    to: compile_id,
    kind: EdgeKind::Always,
    metadata: {},
  }
}
```

What this shows:

- expression closures can still be useful in fallible pipelines
- no `?` is needed
- errors still propagate out of the enclosing rule automatically

This is the main place where Vixen can improve on Rust instead of copying it.

## Sketch 4: Local error handling is explicit

If a rule actually wants to inspect the error locally, that becomes the noisy
path:

```text
fn validate_registry_entry(name: String, version: String) -> Unit throws InputError {
  match catch cargo::registry_package_exists(name, version) {
    Result::Ok(exists) => {
      if !exists {
        fail MissingRegistryCrate {
          name: name,
          version: version,
        }
      }
    }
    Result::Err(err) => {
      fail err
    }
  }
}
```

This is exactly the inversion from Rust:

- propagation is implicit
- local interception is explicit

If the caught result needs to be reused, binding it first is still fine:

```text
result = catch cargo::registry_package_exists(name, version)
match result { ... }
```

But that is not a special limitation of `catch`. The direct `match catch ...`
form should be considered the normal one.

## Sketch 5: Validation stays control-flow-shaped

Not everything should be a pipeline.

```text
fn validate_registry_dependencies(graph: CrateGraph) -> Unit throws InputError {
  for krate in graph.crates {
    match krate.source {
      CrateSource::Registry { name, version, checksum } => {
        exists = cargo::registry_package_exists(name, version)
        if !exists {
          fail MissingRegistryCrate {
            name: name,
            version: version,
          }
        }

        valid = cargo::registry_checksum_matches(name, version, checksum)
        if !valid {
          fail ChecksumMismatch {
            name: name,
            version: version,
          }
        }
      }
      CrateSource::Path { .. } => {}
    }
  }
}
```

This is the important hybrid point:

- transforms should look functional when they are transforms
- validation/error-heavy logic can stay imperative

The language does not need to be ideologically pure.

## Sketch 6: Mutable locals with bang-binding sugar

Some rules will still want small local accumulators.

```text
fn node_metadata(krate: CrateNode) -> {String: String} {
  metadata! = {}
  metadata.insert!("crate_name", krate.crate_name)
  metadata.insert!("crate_type", "bin")
  metadata
}
```

This is much nicer than:

```text
metadata = {}
metadata = metadata.insert(...)
metadata = metadata.insert(...)
```

and does not require making mutation the default.

## Sketch 7: Optional pipeline operator

If this branch becomes central, a pipeline operator may earn its keep:

```text
fn plan_crates(graph: CrateGraph) -> Unit {
  graph.crates
    |> filter(|krate| krate.crate_type == CrateType::Bin)
    |> map(|krate| make_compile_node(krate))
    |> each(|node| emit::node { node })
}
```

This is not necessary for v0, but it is the first extra operator that feels
plausibly justified by the domain.

## Why This Branch Fits Vixen Well

Vixen rule logic is often about:

- selecting subsets
- deriving nodes from inputs
- expanding dependency edges
- transforming graph data

That is naturally expressed as:

- `filter`
- `map`
- `flat_map`
- `fold`
- `each`

not only as:

- `for`
- `if`
- mutable locals

So the functional branch is not aesthetic for its own sake. It matches the
shape of the work.

## The Main Costs

- closures are now in v0
- closure typing needs a real design
- capture semantics need a design
- default propagation from within closures becomes part of the language
  contract
- `throws`-style function signatures need a real surface design
- method-like rebinding sugar for `name.push!(x)` needs explicit rules

Those are real costs. They are not free.

## Current Read

This still feels stronger than the purely imperative branches.

But the good version is not:

- everything becomes a pipeline

It is:

- use functional transforms where the problem is a transform
- use ordinary control flow where the problem is control flow

My strongest current take is:

- expression closures belong earlier than block closures
- removing `?` is probably correct if propagation becomes the default
- `fail` is better than overloading `return Err(...)`
- `name.push!(x)`-style sugar is better than `:=`
- `emit::node { ... }` and `Node { ... }` feel right as one named-payload
  family

The next good questions for this branch are:

1. Should `if` remain at all, or should boolean branching collapse into `match`?
2. Does `name.push!(x)` feel right, or is a different mutable-binding sugar
   cleaner?
3. Should constructors and named-payload calls be unified explicitly in the
   language model?
4. Does the language want a pipeline operator in v0, or is method chaining
   enough?
5. What exact surface should replace `Result<T, E>` and `throws E` in function
   signatures?
