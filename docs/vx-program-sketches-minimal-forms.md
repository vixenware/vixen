# Vixen Program Sketches: Minimal-Forms Variant

This note explores a more holistic direction than the earlier syntax sketches.

The goal here is not "Rust with less punctuation". The goal is:

- a small number of structural forms
- one call model
- one block model
- fewer one-off keywords in headers like `for .. in ..`

The core idea is that loops, conditionals, matches, and function definitions
should all feel like variations on the same grammar, instead of each inventing
their own mini-language.

## Working Kernel

This variant assumes:

- blocks use `do ... end`
- calls always take exactly one explicit payload:
  - positional payload: `f(a, b)`
  - named payload: `f{ key: value }`
- method calls follow the same rule:
  - `value.method(a, b)`
  - `value.method{ key: value }`
- grouping uses `(...)`
- list literals use `[...]`
- record literals use `{...}`
- local binding uses `:=`
- mutable local binding uses `mut name := expr`
- loop headers use a binding arrow instead of `in`

This is trying to minimize grammar by making more constructs share the same
shape.

## The Structural Grammar Bias

The language starts to look like this:

```text
fn name(params) -> Return do
  binding := expr

  if(condition) do
    ...
  end

  for(item <- iterable) do
    ...
  end

  match(value) do
    pattern => expr
    pattern => do
      ...
    end
  end
end
```

What is nice about this:

- `fn`, `if`, `for`, and `match` all take one header payload and one block
- `for` no longer has its own special English grammar
- `let` becomes optional as a surface keyword
- calls and control forms both become "keyword/operator + payload + block"

## Sketch 1: Plan compile nodes

```text
fn plan_crates(graph: CrateGraph) -> Unit do
  for(krate <- graph.crates) do
    if(krate.crate_type == @bin) do
      node_id := format::compile_node_id(
        krate.crate_name,
        krate.crate_type,
      )

      emit::node{
        id: node_id,
        kind: @action,
        label: `compile ${krate.crate_name}`,
        metadata: {
          crate_name: krate.crate_name,
          crate_type: "bin",
        },
      }
    end
  end
end
```

What this shows:

- `for(krate <- graph.crates)` is much more compact than `for krate in ...`
- `emit::node{...}` makes named-argument calls feel first-class
- `:=` carries binding without needing `let`

What feels good:

- the forms are more uniform
- `emit::node{...}` feels very natural for planner operations

What feels risky:

- `:=` is a real departure from Rust
- `emit::node{...}` blurs the line between call and literal in a way that needs
  careful parsing rules

## Sketch 2: Plan compile edges

```text
fn plan_edges(graph: CrateGraph) -> Unit do
  for(krate <- graph.crates) do
    compile_id := format::compile_node_id(
      krate.crate_name,
      krate.crate_type,
    )

    for(dep <- krate.deps) do
      dep_krate := graph.lookup_crate(dep.crate_id)?
      dep_id := format::compile_node_id(
        dep_krate.crate_name,
        dep_krate.crate_type,
      )

      emit::edge{
        from: dep_id,
        to: compile_id,
        kind: @always,
        metadata: {},
      }
    end
  end
end
```

What this shows:

- the loop form scales well to nesting
- namespaced calls with named payloads feel good for emission
- method calls with positional payloads feel good for lookups

## Sketch 3: Validate manifest-derived input

```text
fn validate_registry_dependencies(graph: CrateGraph) -> Result<Unit, InputError> do
  for(krate <- graph.crates) do
    match(krate.source) do
      @registry{name, version, checksum} => do
        exists := cargo::registry_package_exists(name, version)?
        if(!exists) do
          return Err{
            kind: @missing_registry_crate,
            name: name,
            version: version,
          }
        end

        valid := cargo::registry_checksum_matches(name, version, checksum)?
        if(!valid) do
          return Err{
            kind: @checksum_mismatch,
            name: name,
            version: version,
          }
        end
      end

      @path{} => ()
    end
  end

  Ok(())
end
```

What this shows:

- `match(expr) do ... end` composes well with `=>`
- tagged patterns still fit
- the form system stays stable even in branch-heavy code

What feels risky:

- `Err{...}` assumes a named-payload constructor-like call model
- `@path{} => ()` is compact, but maybe too cute

## Sketch 4: Build a filtered list

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] do
  mut out := []

  for(krate <- graph.crates) do
    if(krate.crate_type == @bin) do
      out.push(krate)
    end
  end

  out
end
```

Alternative if collection mutation should not rely on method syntax:

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] do
  mut out := []

  for(krate <- graph.crates) do
    if(krate.crate_type == @bin) do
      list::push(out, krate)
    end
  end

  out
end
```

What this shows:

- `mut out := []` is compact and clear
- the binding/operator story is more uniform than `let mut`
- method calls can exist without defining the whole language around them

## Sketch 5: Construct a compile node

```text
fn make_compile_node(krate: CrateNode) -> Node do
  Node{
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
  }
end
```

What this shows:

- once blocks are `do/end`, ordinary brace records feel fine again
- a conservative record syntax still works inside a more minimal grammar
- this version is far less magical than the Styx-heavy `key value` record form

## Sketch 6: One call model, three surfaces

If calls always take one payload, the language becomes easier to reason about:

```text
format::compile_node_id(name, crate_type)
emit::node{ id: node_id, kind: @action }
graph.lookup_crate(dep.crate_id)
```

That suggests a clean rule:

- `(...)` means positional payload
- `{...}` means named payload
- methods follow the same payload rule as free functions

This is much cleaner than:

- juxtaposition calls for some things
- parens for others
- special statement grammar for a third category

## Why `let` Feels Weak Here

If the language goes in this direction, `let` is one of the least justified
pieces of Rust carryover.

Compare:

```text
let node_id = ...
let mut out = []
```

vs:

```text
node_id := ...
mut out := []
```

The second version:

- is shorter
- makes binding look like one uniform form
- does not require a separate declaration keyword

So my read is:

- `fn`, `if`, `for`, `match`, `return` still earn their keep
- `let` probably does not

## What This Variant Seems To Want

- `do/end` for all blocks
- payload-oriented headers and calls
- `:=` for binding
- `for(pattern <- iterable)` instead of `for pattern in iterable`
- `(...)` and `{...}` as the only two real payload shapes
- tags for compact domain variants

This is not "S-expressions but not really", but it is moving toward:

- a smaller number of syntactic building blocks
- less English in control forms
- less punctuation cargo-culted from Rust

## What Seems Strong

- the forms feel more coherent as a language
- `for(... <- ...)` is much easier to justify than `for ... in ... do`
- `emit::node{...}` is probably the cleanest emission surface so far
- dropping `let` may actually simplify the language meaningfully

## What Still Feels Open

- whether `:=` is the right binding operator
- whether `if(condition)` wants mandatory parens
- whether `match` arms want `=>` or another shape
- whether method mutation is okay as surface syntax
- whether constructors like `Node{...}` and calls like `emit::node{...}` are
  close enough to be a feature, or too close

## Current Read

This is the first sketch family that feels like it has an actual grammar idea,
not just a styling preference.

The strongest moves are:

- `do/end`
- `:=`
- one payload rule for calls and headers

If this branch is worth continuing, the next comparison should probably be:

1. `if(cond) do` vs `if cond do`
2. `node_id := ...` vs `name = ...` with declaration inferred from scope
3. `emit::node{...}` vs `emit::node(Node{...})`
