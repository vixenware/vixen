# Vixen Syntax Design

This document is the working source of truth for the Vixen language surface.

It is intentionally scoped to syntax and front-end structure. It does **not**
authorize semantic implementation against a speculative Kajit HIR. Kajit-backed
typed semantics remain blocked until Kajit has a real HIR crate and internal
producer/consumer path.

## Status

Current intent:

- design the surface language now
- build parser/CST/highlighting/LSP infrastructure now
- do **not** implement the new typed semantic pipeline yet
- do **not** treat `vx-plan`'s current `vx_lang` prototype as the new source of
  truth

## Overall Direction

The language should feel Rust-like, but with sharp limits similar in spirit to
what Starlark is to Python:

- familiar syntax
- much smaller surface
- intentionally restricted power
- deterministic by default
- not Turing-complete by design

The current prototype in [vx_lang.rs](/Users/amos/bearcove/vixen/crates/vx-plan/src/vx_lang.rs)
is a legacy design artifact, not the target language.

## Parsing Architecture

The intended implementation shape is modeled after Styx:

- a dedicated lossless CST built with `rowan`
- a separate tree-sitter grammar for editor integration and highlighting
- LSP features built on the CST, not on tree-sitter

That means:

- tree-sitter is for editor ecosystem support
- the real parser is not tree-sitter
- the CST is the front-end structural backbone

## Syntax Decisions So Far

### General feel

- Rust-like surface syntax
- explicit syntax over clever magic
- familiar punctuation and declarations where possible

### Statements and blocks

- statements end with `;`
- the last expression in a block may omit `;`
- blocks use `{ ... }`
- grouping and calls use `( ... )`
- list literals use `[ ... ]`
- map literals use `{ key: value, ... }`

### Bindings and mutation

- `let` introduces bindings
- assignment is separate from binding
- values are immutable by default
- `mut` is explicit, Rust-style
- field assignment is allowed
- index assignment is allowed
- collection mutation is allowed

Examples:

```rust
let x = 1;
let mut plan = RulePlan::new();
node.label = "compile";
items[i] = value;
```

### Collections

- list literals use `[ ... ]`
- map literals use `{ key: value, ... }`
- `[]` is reserved for list indexing
- maps should use methods such as `get`, `insert`, and `remove`

Current bias:

- avoid ambiguous missing-key behavior for maps in v0
- defer set literal syntax until there is a real need

### Strings

- ordinary string literals use double quotes
- template strings use backticks
- interpolation uses TypeScript-style `${expr}`

Examples:

```rust
"compile"
`compile ${krate.crate_name}`
```

Current bias:

- template strings should be the preferred ergonomic way to build labels and
  similar user-facing strings
- `format::concat(...)`-style construction should not be the default surface
  style

### Functions

- functions use `fn`
- type annotations look Rust-like
- local type inference is allowed where obvious

Example:

```rust
fn plan_crates(graph: CrateGraph) -> RulePlan {
    graph.into_plan()
}
```

### Types

- user-defined `struct` is wanted
- user-defined `enum` is wanted
- `Option` and `Result` are in
- type syntax should look Rust-like
- no trait machinery in v0

### Control flow

- `if`
- `for .. in ..`
- `match`
- `return`

`match` should support enum destructuring early enough to make enums actually
usable.

### Methods

- method-call syntax is wanted

Current assumption:

- methods use value receivers, not references
- `self` is just a normal receiver binding
- `mut self` means the receiver binding is mutable inside the method body
- methods in v0 are surface sugar over statically-known builtins or host/domain
  operations, unless/until real method definitions are added
- `.` on values is primarily field access
- if method syntax exists, it should desugar cleanly
- user-defined method definitions are still deferred

Example:

```rust
deps.len()
graph.root()
```

Receiver mutability should follow the ordinary binding rules:

```rust
fn insert(mut self, key: String, value: String) -> Self {
    self
}

fn rewrite(self) -> Self {
    let mut this = self;
    this
}
```

This does **not** imply traits, references, or full Rust-style method
resolution in v0.

### Namespaces

- namespace-qualified items use Rust-style `::`
- `.` is reserved for field access and method calls on values

Examples:

```rust
format::compile_node_id(name, crate_type)
emit::node(node)
env::profile()
```

This keeps call syntax unambiguous:

- `foo.bar` means field or method
- `foo::bar(...)` means namespaced item

### Error handling

- `Option`
- `Result`
- `match`
- `return`
- `?` is acceptable in v0 if it is only exact-type early-return sugar

Current bias:

- support straightforward typed error handling
- avoid Rust trait-heavy conversion machinery
- no exception model in v0 unless a compelling reason appears

## Explicit v0 Restrictions

These restrictions are intentional, not temporary accidents.

- no recursion
- no `while`
- no bare `loop`
- no closures in v0
- no traits in v0
- no imports in v0
- no references (`&`, `&mut`) in v0
- no set literal syntax in v0
- no bitwise operators in v0 unless a real need appears

Termination constraint:

- the language is intended to be non-Turing-complete
- iteration must be over finite collections or other obviously bounded domains

## Multi-file and Modules

This is still open, but the current position is:

- multi-file will probably matter
- imports/modules are not part of the first parser pass
- do not design the first syntax pass around a module system yet

## Semantics Deferred on Purpose

The following are deliberately **not** being committed yet:

- typed lowering to Kajit HIR
- interpreter semantics for the new language
- final builtin execution model
- ownership/borrowing semantics beyond ordinary mutable bindings
- exact module/import model

## Value Semantics

Current design bias:

- the language has value semantics
- the language does **not** expose mutable references or aliasing semantics
- methods and functions conceptually receive values, not references

That does **not** mean implementations must perform deep copies.

Implementation freedom should include:

- structural sharing
- copy-on-write
- uniqueness-based in-place mutation
- field-splitting
- scalar replacement
- other equivalent optimizations

The rule is:

- programs behave *as if* values are passed and returned by value
- implementations may optimize representation and update strategy as long as the
  observable behavior matches value semantics

This matters especially for large collections used in build planning. The
language model should stay simple even when the runtime representation is highly
optimized.

## Capabilities

This needs a dedicated follow-up discussion.

Current high-level view:

- rules mostly describe build steps, graph nodes, and edges
- the language should not casually expose ambient side effects
- deterministic/default-safe behavior matters

But the exact capability model should be documented separately from syntax.

## Open Questions

1. Do we eventually want limited inherent `impl` blocks without traits, or keep
   methods as sugar over builtins/domain ops for longer?
2. What is the smallest acceptable module system once multi-file becomes
   necessary?
3. What collection APIs exist in v0 if closures are absent?
4. What operators are actually needed for real rule code, beyond arithmetic,
   comparison, and boolean logic?
5. What syntax should construct sets, if/when sets become necessary?

## Next Safe Work

Safe to build now:

- syntax examples and corpus
- `rowan` CST crate
- tree-sitter grammar
- syntax highlighting queries
- parser diagnostics and spans
- CST-backed LSP features

Not safe yet:

- new typed semantic IR
- new interpreter
- Kajit-targeted lowering
