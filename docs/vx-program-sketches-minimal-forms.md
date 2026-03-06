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
- immutable local binding uses `name = expr`
- mutable local introduction or shadowing uses `name! = expr`
- loop headers use a binding arrow instead of `in`

This is trying to minimize grammar by making more constructs share the same
shape.

## The Structural Grammar Bias

The language starts to look like this:

```text
fn name(params) -> Return do
  binding = expr

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
- `let` disappears as a surface keyword
- calls and control forms both become "keyword/operator + payload + block"

## Sketch 1: Plan compile nodes

```text
fn plan_crates(graph: CrateGraph) -> Unit do
  for(krate <- graph.crates) do
    if(krate.crate_type == @bin) do
      node_id = format::compile_node_id(
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
- plain `=` carries immutable binding without needing `let`

What feels good:

- the forms are more uniform
- `emit::node{...}` feels very natural for planner operations

What feels risky:

- `name!` needs a very clear "this binding is mutable" rule
- `emit::node{...}` blurs the line between call and literal in a way that needs
  careful parsing rules

## Sketch 2: Plan compile edges

```text
fn plan_edges(graph: CrateGraph) -> Unit do
  for(krate <- graph.crates) do
    compile_id = format::compile_node_id(
      krate.crate_name,
      krate.crate_type,
    )

    for(dep <- krate.deps) do
      dep_krate = graph.lookup_crate(dep.crate_id)?
      dep_id = format::compile_node_id(
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
        exists = cargo::registry_package_exists(name, version)?
        if(!exists) do
          return Err{
            kind: @missing_registry_crate,
            name: name,
            version: version,
          }
        end

        valid = cargo::registry_checksum_matches(name, version, checksum)?
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
  out! = []

  for(krate <- graph.crates) do
    if(krate.crate_type == @bin) do
      out!.push(krate)
    end
  end

  out
end
```

Alternative if collection mutation should not rely on method syntax:

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] do
  out! = []

  for(krate <- graph.crates) do
    if(krate.crate_type == @bin) do
      list::push(out, krate)
    end
  end

  out
end
```

What this shows:

- `out! = []` introduces a mutable local without another keyword
- `out!.push(krate)` reads like mutation of that local binding
- immutable bindings stay quiet while mutable writes stand out
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

## Why `let` and `:=` Feel Weak Here

If the language goes in this direction, `let` is one of the least justified
pieces of Rust carryover.

Compare:

```text
let node_id = ...
let mut out = []
```

vs:

```text
node_id = ...
out! = []
```

The second version:

- is shorter
- makes immutability the quiet default
- makes mutability explicit at the write site
- does not require a separate declaration keyword

So my read is:

- `fn`, `if`, `for`, `match`, `return` still earn their keep
- `let` and `:=` probably do not

## What This Variant Seems To Want

- `do/end` for all blocks
- payload-oriented headers and calls
- `=` for immutable binding
- `name! = expr` for mutable local introduction/shadowing
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
- dropping `let` and `:=` may actually simplify the language meaningfully

## What Still Feels Open

- whether `name!.push(x)` should be the canonical mutation sugar, or whether
  mutation should stay as explicit rebinding
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
- plain `=` for immutable bindings
- `name! = expr` for mutable writes
- one payload rule for calls and headers

If this branch is worth continuing, the next comparison should probably be:

1. `if(cond) do` vs `if cond do`
2. `name!.push(x)` vs explicit rebinding
3. `emit::node{...}` vs `emit::node(Node{...})`
