# Vixen Syntax Design

This document is the working source of truth for the Vixen language surface.

It is intentionally scoped to syntax and front-end structure. It does **not**
authorize semantic implementation against a speculative Kajit HIR. Kajit-backed
typed semantics remain blocked until Kajit has a real HIR crate and internal
producer/consumer path.

Styx-inspired simplification experiments are tracked separately in
[vx-styx-inspiration.md](/Users/amos/bearcove/vixen/docs/vx-styx-inspiration.md),
but the active sketch direction is now the functional branch in
[vx-program-sketches-functional.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches-functional.md).

## Status

Current intent:

- design the surface language now
- build parser/CST/highlighting/LSP infrastructure now
- do **not** implement the new typed semantic pipeline yet
- do **not** treat `vx-plan`'s current `vx_lang` prototype as the new source of
  truth

The current design direction is:

- no `do/end`
- no `?`
- no Styx-style `@tag` values
- no separate mutable-binding sigil
- method chaining instead of a pipeline operator, for now
- default error propagation with explicit `catch` / `fail`
- `if` for simple boolean guards
- `match` for pattern matching and multi-way guard branching
- `for` for bounded imperative iteration
- no bare `loop`
- short unqualified variant/constructor forms inferred from type context

## Parsing Architecture

The intended implementation shape is modeled after Styx:

- a dedicated lossless CST built with `rowan`
- a separate tree-sitter grammar for editor integration and highlighting
- LSP features built on the CST, not on tree-sitter

That means:

- tree-sitter is for editor ecosystem support
- the real parser is not tree-sitter
- the CST is the front-end structural backbone

## Surface Model

### Blocks, values, and punctuation

- ordinary blocks use `{ ... }`
- bare `{ ... }` in expression position is a map/object literal
- bare `{}` in expression position means an empty map/object
- `()` is the unit value
- list literals use `[ ... ]`
- template strings use backticks with `${expr}` interpolation

Examples:

```text
metadata = { crate_name: krate.crate_name, crate_type: "bin" }
unit = ()
label = `compile ${krate.crate_name}`
```

The exact punctuation policy is still open. Current sketches intentionally use
light punctuation and usually omit semicolons.

### Bindings, shadowing, and places

Plain assignment always introduces a fresh binding, even in the same scope.

```text
x = 1
x = x + 1
```

This means `=` is binding/shadowing, not slot update. That matches the current
value-semantics direction.

Explicit place updates still exist:

```text
node.label = "compile"
items[i] = value
```

Method-like writeback uses a bang to mark the hidden rewrite:

```text
items.push!(node)
map.insert!(key, value)
graph.crates.push!(node)
```

The intended reading is:

```text
items = items.push(node)
map = map.insert(key, value)
graph.crates = graph.crates.push(node)
```

Current rules:

- `place.method!(...)` is only valid for a place expression
- the rewritten call must return **exactly** the same type as the place
- plain field/index assignment stays explicit and does not need `!`
- because `=` shadows, closures capture values, not shared mutable cells

There is currently no separate `name! = expr` mutable-binding form.

### Constructors, variants, and type-directed short forms

Both qualified and short forms are legal:

```text
CrateType::Bin
Bin

CrateSource::Registry { name, version, checksum }
Registry { name, version, checksum }
```

The rule is:

- short forms are allowed in any position
- the compiler uses whole-program type information to infer the missing enum or
  type name
- inference only succeeds when there is a real source of expected type and the
  result is unambiguous
- otherwise it is an error and the user must write the qualified form

This is **not** a namespace search rule for ordinary calls. It is specifically
about type-directed resolution of constructors and variants.

The same construction surface is used for structs and enum variants:

```text
Node { id, kind, label }
Registry { name, version, checksum }
```

`Foo::Bar` is also a real type:

```text
enum Foo {
  Bar { x: U32 }
}

bar: Foo::Bar = Bar { x: 1 }
value: Foo = bar
```

Current intent:

- a variant type coerces to its parent enum where needed
- refinement in the other direction uses `as`

```text
bar = value as Foo::Bar
```

which returns `Option<Foo::Bar>`.

### Maps and objects

Plain brace literals are ordinary maps/objects and can omit quotes around
identifier keys:

```text
metadata = { crate_name: krate.crate_name, crate_type: "bin" }
```

Typed construction uses the same field surface with a leading type or variant:

```text
Node { id, kind, label, metadata }
Registry { name, version, checksum }
```

This keeps the parser and the surface coherent:

- `{ foo: bar }` is a plain object/map literal
- `Node { foo: bar }` is typed construction

### Functions, closures, and methods

Functions use `fn` and require parameter annotations:

```text
fn make_node(krate: CrateNode) -> Node { ... }
```

Closures use arrow syntax and infer parameter types from context:

```text
(x) => x.name
(x, y) => combine(x, y)
() => None
```

In unary expression-closure positions, `$` may stand for the implicit argument:

```text
graph.crates.filter($.crate_type == Bin)
numbers.map(2 * $)
```

This is shorthand for:

```text
graph.crates.filter((krate) => krate.crate_type == Bin)
numbers.map((x) => 2 * x)
```

Current bias:

- `$` is only shorthand for unary expression closures
- block-bodied closures still spell their parameters explicitly
- multi-parameter closures still spell their parameters explicitly

Both single-expression and block-bodied closures are allowed:

```text
(dep) => edge_for_dep(graph, compile_id, dep)

(dep) => {
  dep_krate = graph.lookup_crate(dep.crate_id)
  Edge { ... }
}
```

The last expression in a block-bodied closure is its value by default.

Closures are non-escaping in v0:

- they are intended for immediate inline use in combinators and similar calls
- they are not stored, returned, or invoked later
- this keeps nonlocal `return` well-defined

Methods are surface sugar over ordinary functions. There is no distinct method
declaration form in v0.

```text
x.f(y, z)
```

desugars to:

```text
f(x, y, z)
```

Current working assumptions:

- method lookup is static
- any **visible** function/prelude item whose first parameter matches the
  receiver type may be called with method syntax
- if multiple candidates fit, it is an ambiguity error
- if none fit, it is an error
- there is no dynamic dispatch, trait machinery, or overloading-based method
  system in v0

### Control flow

The current direction keeps both `if` and `match`, because they serve different
shapes of logic.

Use `if` for ordinary single-condition branching:

```text
if !exists {
  fail MissingRegistryCrate { name, version }
}
```

Use `match` for pattern matching and multi-way branching:

There are two distinct `match` forms.

Scrutinee match:

```text
match source {
  Registry { name, version, checksum } => ...
  Path { path } => ...
  _ => ...
}
```

Guard-only branching:

```text
match {
  x > 0 => positive
  x < 0 => negative
  _ => zero
}
```

Rules:

- `if` remains available in v0
- `for` remains available for bounded iteration over finite collections
- scrutinee match and guard-only match are separate forms
- scrutinee-match arms do **not** have extra guards in v0
- guard-only match is checked top-to-bottom; the first matching arm wins
- `_` is the fallback arm
- patterns support literals plus record/variant destructuring
- field punning and rename are allowed

Examples:

```text
Registry { name, version }
Registry { name: crate_name, version }
Some(value)
Ok(result)
Err(err)
```

Exhaustiveness is intentionally conservative for now. The current bias is:

- require `_` unless coverage is locally obvious enough to prove cheaply
- require bounded iteration sources for `for`

### Breakable scopes, `return`, and `continue`

Blocks, closures, and bounded `for` loops are breakable.

```text
@choose {
  break choose 42
}
```

Labels use an `@name` marker on the target and a plain name in the `break`:

```text
@choose {
  break choose 42
}
```

Current intent:

- plain `break value` exits the nearest breakable thing
- closures are breakable too
- `continue` is `for`-loop-only
- `return value` exits the enclosing function, even from inside a closure

That gives a useful split:

- `break value` is local to the nearest breakable scope/closure/loop
- `return value` is non-local to the enclosing function

## Errors and Absence

Errors are surfaced at function boundaries with `throws`:

```text
fn plan_edges(graph: CrateGraph) -> Unit throws InputError { ... }
```

Inside a throwing function:

- fallible calls yield their success value on the happy path
- errors propagate automatically by default
- `fail err` exits the current `throws` boundary with an error
- `catch expr` suppresses propagation and yields an ordinary `Result`

For closures:

- a closure body inherits the enclosing `throws` context
- combinators are typed over whether their closure body may fail
- a combinator using a fallible closure becomes fallible itself
- that fallibility then participates in the ordinary default-propagation model

Example:

```text
dep_krate = graph.lookup_crate(dep.crate_id)

match catch cargo::registry_package_exists(name, version) {
  Ok(exists) => ...
  Err(err) => fail err
}
```

Absence is modeled with `Option`, not `null`.

```text
None
Some(value)
```

`or` has `??`-style semantics:

- it falls back only on absence
- it does **not** treat `false` or `0` as absent

Examples:

```text
name = maybe_name or "default"
registry = value as CrateSource::Registry or fail ExpectedRegistrySource { value }
```

### Result and Option surface

`Option` and `Result` keep Rust-style variants:

```text
Some(value)
None
Ok(value)
Err(err)
```

Pattern matching uses those variants directly because they are ordinary enums.

The default vocabulary/prelude should also make these ordinary helper methods
available:

```text
value.some()
value.ok()
value.err()
```

These are intended as ordinary prelude methods, not magical syntax forms.

## Default Vocabulary

Vixen should have a real default vocabulary.

Current bias:

- collection combinators and utilities are in the prelude
- method chaining is the primary composition style
- no pipeline operator in v0

Examples of the intended feel:

```text
graph.crates
  .filter((krate) => krate.crate_type == Bin)
  .map((krate) => make_compile_node(krate))
  .each((node) => emit::node { node })
```

## Explicit v0 Restrictions

These restrictions are intentional, not temporary accidents.

- no recursion
- no `while`
- no bare `loop`
- no references (`&`, `&mut`) in v0
- no traits / impl blocks / dynamic dispatch in v0
- no pipeline operator in v0

Termination constraint:

- the language is intended to be non-Turing-complete
- iteration must be over finite collections or other obviously bounded domains

## Still Open

- exact module/import system
- exact punctuation policy, including semicolons and commas
- casing/style conventions for constructors and variants
- exact strength of v0 exhaustiveness checking
- whether method lookup should stay strictly limited to visible names plus the
  prelude, or gain a broader discovery rule later
