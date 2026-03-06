# Vixen Program Sketches: Functional Variant

This note explores a different turn from the earlier grammar sketches.

The question here is not:

- how do we make `for` cleaner?

It is:

- what if Vixen wants collection transforms, closures, and expression-oriented
  rule code badly enough that imperative loops stop being the center of the
  language?

This is closer to:

- a typed rule DSL
- with functional collection programming
- still constrained and deterministic
- but less statement-driven

## Working Assumptions

This variant assumes:

- blocks still use `do ... end`
- closures exist
- the main closure forms are:
  - expression closure: `|x| expr`
  - block closure: `|x| do ... end`
- collection APIs like `map`, `filter`, `flat_map`, `fold`, and `each` are
  first-class
- recursion is still out
- higher-order collection operations are still bounded by the input collection

This is trying to keep the language non-Turing-complete while letting it feel
much more like "transform data into a plan".

## Closure Surface

The smallest plausible closure surface is probably:

```text
|krate| krate.crate_type == @bin
|krate| Node{ ... }
|left, right| left.merge(right)
```

And when a body wants multiple steps:

```text
|krate| do
  node_id := format::compile_node_id(
    krate.crate_name,
    krate.crate_type,
  )

  Node{
    id: node_id,
    kind: @action,
    label: `compile ${krate.crate_name}`,
  }
end
```

This is the first place where `do/end` really helps. It gives closures a block
form without reusing braces.

## Sketch 1: Plan compile nodes by mapping first, emitting second

```text
fn plan_crates(graph: CrateGraph) -> Unit do
  graph.crates
    .filter(|krate| krate.crate_type == @bin)
    .map(|krate| do
      node_id := format::compile_node_id(
        krate.crate_name,
        krate.crate_type,
      )

      Node{
        id: node_id,
        kind: @action,
        label: `compile ${krate.crate_name}`,
        metadata: {
          crate_name: krate.crate_name,
          crate_type: "bin",
        },
      }
    end)
    .each(|node| emit::node{ node: node })
end
```

What this shows:

- the rule reads as a transformation pipeline, not an instruction list
- `filter` and `map` match the problem better than nested `if` inside `for`
- `emit::node{...}` still works as the side-effect boundary

What feels good:

- this is much closer to "what the program means"
- the sequence of operations is easier to scan than the imperative version

What feels risky:

- the surface now depends heavily on closures being pleasant
- long chains may want a pipeline operator later

## Sketch 2: Plan edges with `flat_map`

```text
fn plan_edges(graph: CrateGraph) -> Unit do
  graph.crates
    .flat_map(|krate| do
      compile_id := format::compile_node_id(
        krate.crate_name,
        krate.crate_type,
      )

      krate.deps.map(|dep| do
        dep_krate := graph.lookup_crate(dep.crate_id)?
        dep_id := format::compile_node_id(
          dep_krate.crate_name,
          dep_krate.crate_type,
        )

        Edge{
          from: dep_id,
          to: compile_id,
          kind: @always,
          metadata: {},
        }
      end)
    end)
    .each(|edge| emit::edge{ edge: edge })
end
```

What this shows:

- nested graph expansion is a real use case for `flat_map`
- closure blocks make local helper bindings okay
- the program looks more like graph derivation than step-by-step mutation

What feels risky:

- this may be too abstract for some rules
- if `?` is allowed inside closures, return behavior needs to be very clear

## Sketch 3: Validate manifest-derived input as a diagnostic pipeline

```text
fn validate_registry_dependencies(graph: CrateGraph) -> Result<Unit, InputError> do
  graph.crates
    .map(|krate| match(krate.source) do
      @registry{name, version, checksum} => do
        exists := cargo::registry_package_exists(name, version)?
        if(!exists) do
          Err{
            kind: @missing_registry_crate,
            name: name,
            version: version,
          }
        end else do
          valid := cargo::registry_checksum_matches(name, version, checksum)?
          if(!valid) do
            Err{
              kind: @checksum_mismatch,
              name: name,
              version: version,
            }
          end else do
            Ok(())
          end
        end
      end

      @path{} => Ok(())
    end)
    .collect_results()?

  Ok(())
end
```

What this shows:

- validation logic can still be expression-oriented
- a helper like `collect_results()` starts to matter
- `match` as an expression becomes more important than `match` as a statement

What feels good:

- this makes "validate every crate" read like validation, not control flow

What feels risky:

- this may be pushing too hard toward combinator-heavy code
- `if ... else do ... end` needs a clean expression form if this branch is real

## Sketch 4: Build a filtered list directly

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] do
  graph.crates.filter(|krate| krate.crate_type == @bin)
end
```

This is the clearest argument for the whole branch.

Compared to the earlier imperative sketches, this is simply the right shape for
the problem.

## Sketch 5: Build a plan fragment with `fold`

```text
fn plan_fragment(graph: CrateGraph) -> RulePlan do
  graph.crates
    .filter(|krate| krate.crate_type == @bin)
    .map(|krate| Node{
      id: format::compile_node_id(
        krate.crate_name,
        krate.crate_type,
      ),
      kind: @action,
      label: `compile ${krate.crate_name}`,
      metadata: {
        crate_name: krate.crate_name,
        crate_type: "bin",
      },
    })
    .fold(RulePlan::new(), |plan, node| plan.add_node(node))
end
```

What this shows:

- if Vixen ever wants plan fragments as values, `fold` is the natural tool
- value semantics and functional transforms fit each other well

What feels risky:

- `fold` is powerful, but it can also become a dumping ground for logic that
  should have stayed clearer

## Sketch 6: Functional emission with a pipeline operator

If the language leans this way, it may want a pipeline operator badly.

```text
fn plan_crates(graph: CrateGraph) -> Unit do
  graph.crates
    |> filter(|krate| krate.crate_type == @bin)
    |> map(|krate| Node{
         id: format::compile_node_id(
           krate.crate_name,
           krate.crate_type,
         ),
         kind: @action,
         label: `compile ${krate.crate_name}`,
       })
    |> each(|node| emit::node{ node: node })
end
```

This is not necessary, but it may become the cleanest surface if the language
is genuinely pipeline-oriented.

## What This Variant Seems To Want

- closures as real surface syntax
- `match` and `if` usable as expressions, not just statements
- collection combinators as core language affordances
- side effects isolated at the edge (`emit`, diagnostics, host queries)
- maybe a pipeline operator
- maybe fewer explicit local bindings overall

This is a more ambitious language than the earlier minimal-form sketch.

It is less "small control-flow DSL" and more "typed transformational language".

## Why This May Actually Fit Vixen Better

Vixen rules are often about:

- selecting subsets
- rewriting graph-shaped data
- deriving nodes and edges
- validating collections of related inputs

That is naturally expressed as:

- filter
- map
- flat_map
- fold
- partition
- collect_results

not just as:

- `for`
- `if`
- mutable locals

So if the language is meant to describe rule logic rather than systems code,
this branch may be closer to the true center.

## The Main New Costs

- closures must exist
- capture semantics must be designed
- `?` inside closures must be specified carefully
- the language may become more abstract than some users want
- good names for combinators matter a lot

And most importantly:

- this branch reopens the decision to ban closures in v0

## Current Read

This feels more like a real candidate than I expected.

Not because imperative code is bad, but because the actual Vixen problems are
heavily collection-transform and graph-derivation shaped.

My strongest current take is:

- if Vixen goes functional, closures become more justified than methods

The next questions for this branch are probably:

1. What is the smallest closure syntax that still feels good?
2. Is `if` an expression, a statement, or both?
3. Does the language want a pipeline operator?
4. Does `emit` stay side-effectful, or do we prefer returning fragments and
   combining them?
