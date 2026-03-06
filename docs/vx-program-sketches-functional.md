# Vixen Program Sketches: Functional Variant

This note explores a more functional direction for Vixen.

The point is not "replace all control flow with combinators". The point is:

- make collection transforms first-class
- let rule code read more like graph/data derivation
- avoid encoding obvious transforms as manual accumulation loops

This branch has moved away from the earlier assumptions:

- no `do/end`
- no `:=`
- no block closures in v0

## Working Assumptions

This variant assumes:

- ordinary blocks still use `{ ... }`
- closures exist in v0
- v0 closures are expression closures only: `|x| expr`
- collection APIs like `map`, `filter`, `flat_map`, `fold`, and `each` are
  first-class
- recursion is still out
- higher-order collection operations are still bounded by input collections
- `?` is allowed inside closures

The important non-Rust rule is:

- `?` inside a closure propagates out of the enclosing fallible rule, not out
  of the closure itself

That is possible because Vixen does not need Rust's "closures are just ordinary
functions" rule.

## Binding and Mutation Surface

Current bias for this branch:

```text
node_id = format::compile_node_id(name, ty)
nodes! = []
nodes!.push(node)
```

With the reading:

- `name = expr` introduces an immutable binding
- `name! = expr` introduces a mutable binding, possibly by shadowing
- `name!.method(args)` is sugar for rebinding the current mutable `name`

So:

```text
nodes! = []
nodes!.push(node)
nodes!.push(other)
```

behaves like repeated rebinding of the same mutable local, without needing
`let mut` or `nodes = nodes.push(...)`.

This keeps:

- immutable bindings quiet
- mutation explicit
- `!` attached to the mutable binding, not to method names

## Closure Surface

The smallest plausible closure surface is probably:

```text
|krate| krate.crate_type == @bin
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
  graph.crates.filter(|krate| krate.crate_type == @bin)
}
```

This is still the clearest argument for the whole branch.

Compared to the imperative form, this is simply closer to what the program
means.

## Sketch 2: Map nodes, then emit them

```text
fn plan_crates(graph: CrateGraph) -> Unit {
  graph.crates
    .filter(|krate| krate.crate_type == @bin)
    .map(|krate| make_compile_node(krate))
    .each(|node| emit::node{node})
}

fn make_compile_node(krate: CrateNode) -> Node {
  Node{
    id: format::compile_node_id(krate.crate_name, krate.crate_type),
    kind: @action,
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
- expression closures are enough if helper extraction is cheap

## Sketch 3: `?` inside closures, but not Rust-style

```text
fn plan_edges(graph: CrateGraph) -> Result<Unit, InputError> {
  graph.crates
    .flat_map(|krate| edges_for_crate(graph, krate)?)
    .each(|edge| emit::edge{edge})

  Ok(())
}

fn edges_for_crate(graph: CrateGraph, krate: CrateNode) -> Result<[Edge], InputError> {
  compile_id = format::compile_node_id(krate.crate_name, krate.crate_type)

  krate.deps.map(|dep| edge_for_dep(graph, compile_id, dep)?)
}

fn edge_for_dep(graph: CrateGraph, compile_id: NodeId, dep: DepEdge) -> Result<Edge, InputError> {
  dep_krate = graph.lookup_crate(dep.crate_id)?
  dep_id = format::compile_node_id(dep_krate.crate_name, dep_krate.crate_type)

  Ok(Edge{
    from: dep_id,
    to: compile_id,
    kind: @always,
    metadata: {},
  })
}
```

What this shows:

- expression closures can still be useful in fallible pipelines
- `?` inside `flat_map` and `map` closures can mean what users actually want
- helper extraction keeps closure syntax small without losing the functional
  shape

This is the main place where Vixen can improve on Rust instead of copying it.

## Sketch 4: Validation stays control-flow-shaped

Not everything should be a pipeline.

```text
fn validate_registry_dependencies(graph: CrateGraph) -> Result<Unit, InputError> {
  for krate in graph.crates {
    match krate.source {
      @registry{name, version, checksum} => {
        exists = cargo::registry_package_exists(name, version)?
        if !exists {
          return Err{
            kind: @missing_registry_crate,
            name: name,
            version: version,
          }
        }

        valid = cargo::registry_checksum_matches(name, version, checksum)?
        if !valid {
          return Err{
            kind: @checksum_mismatch,
            name: name,
            version: version,
          }
        }
      }
      @path{} => {}
    }
  }

  Ok(())
}
```

This is the important hybrid point:

- transforms should look functional when they are transforms
- validation/error-heavy logic can stay imperative

The language does not need to be ideologically pure.

## Sketch 5: Mutable locals with bang-binding sugar

Some rules will still want small local accumulators.

```text
fn node_metadata(krate: CrateNode) -> {String: String} {
  metadata! = {}
  metadata!.insert("crate_name", krate.crate_name)
  metadata!.insert("crate_type", "bin")
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

## Sketch 6: Optional pipeline operator

If this branch becomes central, a pipeline operator may earn its keep:

```text
fn plan_crates(graph: CrateGraph) -> Unit {
  graph.crates
    |> filter(|krate| krate.crate_type == @bin)
    |> map(|krate| make_compile_node(krate))
    |> each(|node| emit::node{node})
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
- `?` propagation through closures becomes part of the language contract
- method-like rebinding sugar for `name!.push(x)` needs explicit rules

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
- `?` inside closures is worth designing properly
- `name!.push(x)`-style sugar is better than `:=`
- `emit::node{...}` remains one of the strongest surface ideas across all
  branches

The next good questions for this branch are:

1. Should `if` remain at all, or should boolean branching collapse into `match`?
2. Does `name!.push(x)` feel right, or is a different mutable-binding sugar
   cleaner?
3. Should constructors and named-payload calls be unified explicitly in the
   language model?
4. Does the language want a pipeline operator in v0, or is method chaining
   enough?
