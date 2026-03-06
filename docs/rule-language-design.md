# Vixen Rule Language: Design Document

This document proposes the design for the next-generation Vixen rule language,
replacing the current `vx-plan` prototype frontend while preserving and
extending its useful substrate.

## Status Quo

The current `vx-plan` crate contains three layers:

1. **RulePlan IR** (`lib.rs`) — typed graph nodes, edges, facts, provenance,
   validation, canonical JSON serialization. This is the contract between
   planning and execution.

2. **Planner runtime** (`runtime.rs`) — capability-gated interpreter with
   `PlannerHost` trait, scoped variables, `PlannerValue` dynamic type,
   `PlannerBuiltinId` enum, arg parsing, and `RulePlanHost` default
   implementation. ~1700 lines.

3. **vx-lang v0 frontend** (`vx_lang.rs`) — hand-rolled lexer and parser that
   produces `PlannerProgram` (a list of `PlannerExpr`). ~950 lines.

The frontend parses directly into the runtime's AST types (`PlannerExpr`,
`PlannerCall`, `PlannerValue`). There is no typed intermediate representation
between parsing and interpretation. The value system is dynamically typed: a
single `PlannerValue` enum covers null, bool, int, string, keyword, vector,
set, and map. Builtins receive `PlannerBuiltinArgs` which is parsed from
`PlannerValue` maps at call time — essentially runtime type-checking of
stringly-typed arguments.

This was the right thing to build first. It proved the pipeline works end to
end: surface syntax -> interpreter -> RulePlan -> ActionGraph lowering ->
execution. But it has real limitations that will compound as the language grows.

## What to Keep

### RulePlan IR (keep as-is)

The `RulePlan` schema is solid:

- `RulePlanNode` with typed `RulePlanNodeKind` (Action, Artifact, Fact, Group)
- `RulePlanEdge` with typed `RulePlanEdgeKind` (IfChanged, IfWritten, IfCreate,
  IfDeleted, Always)
- `RulePlanFact` with node reference and string value
- `RulePlanProvenance` with rule ID, source ID, span, comment, metadata
- `RulePlanSource` interned source table
- Canonical JSON serialization and structural validation

This is the output contract. Nothing here needs to change.

### ActionGraph lowering bridge (keep as-is)

`ActionGraph::from_rule_plan()` and `from_rule_plan_executable()` work. The
parity harness (`compare_rule_plan_parity`) works. The `PlannerSynthetic` and
`PlannerCompileRustCrate` action variants exist and are wired through the
executor. This bridge is the integration seam — it stays.

### Capability/policy model (keep, extend)

`PlannerCapability` and `PlannerPolicy` are the right abstraction. The
deterministic policy (EmitGraph + Transform + Env) is the right default. The
capability enum will grow but the model is correct.

### PlannerHost trait (keep, retype)

The host trait pattern is right — the runtime delegates side effects to a host
implementation. But the current signature passes `PlannerBuiltinArgs` (a
stringly-typed enum). The new design will pass typed arguments directly.

### Lexer infrastructure (keep, extend)

The hand-rolled lexer in `vx_lang.rs` is clean and handles comments, string
escapes, integers, identifiers, and all the bracket types. It will need new
token kinds (chevrons, `->`, `fn`, `struct`, `enum`, `match`, etc.) but the
architecture is fine.

## What to Rewrite

### Parser -> typed IR (new)

The current parser produces `PlannerExpr` directly. The new parser will produce
a **typed AST** that goes through name resolution and type checking before
reaching the interpreter or Kajit.

### Value system (replace)

`PlannerValue` is a dynamically typed grab-bag. The new language has a real type
system where planner/domain types are Rust-shaped structs and enums. Values at
runtime are still represented as a `Value` enum internally, but the type checker
ensures you never pass an `EdgeKind` where a `NodeKind` is expected.

### Builtin arg parsing (replace)

The current `parse_builtin_args` function is 200+ lines of manual map key
extraction with string error messages. With typed function signatures, this
disappears — the type checker validates call sites statically, and the
interpreter destructures typed arguments directly.

### PlannerExpr AST (replace)

The current `PlannerExpr` conflates surface syntax, IR, and runtime
representation. The new design separates:

1. **Surface AST** — what the parser produces
2. **Typed IR** — what the type checker produces (name-resolved, type-annotated)
3. **Runtime representation** — what the interpreter evaluates

### Dollar-sign variable references (replace)

`$var` syntax is a holdover from the EDN-inspired prototype. Variables are just
identifiers in the new language: `let x = 5; x + 1`.

## Proposed Core Types

### Type system

The language has a structural type system with nominally-typed domain objects.
Chevrons (`<>`) for generic type arguments, matching Rust convention.

```
// Primitive types
Bool
Int
String

// Collection types
List<T>
Map<K, V>
Set<T>
Option<T>

// Domain types — Rust-shaped structs and enums
struct NodeId { value: String }
struct FactId { value: String }

enum NodeKind {
    Action,
    Artifact,
    Fact,
    Group,
}

enum EdgeKind {
    IfChanged,
    IfWritten,
    IfCreate,
    IfDeleted,
    Always,
}

struct Node {
    id: NodeId,
    kind: NodeKind,
    label: String,
}

struct Edge {
    from: NodeId,
    to: NodeId,
    kind: EdgeKind,
}

struct Fact {
    id: FactId,
    node_id: NodeId,
    value: String,
}

// Rust domain types (provided by the rust rule pack)
struct CrateId { value: String }

enum CrateType {
    Lib,
    Bin,
    ProcMacro,
    CDylib,
    StaticLib,
}

struct Crate {
    id: CrateId,
    name: String,
    version: String,
    edition: String,
    crate_type: CrateType,
    deps: List<CrateDep>,
}

struct CrateDep {
    crate_id: CrateId,
    extern_name: String,
}

struct CrateGraph {
    root: CrateId,
    crates: List<Crate>,
    topo: List<CrateId>,
}
```

These types are **the same shape as the Rust structs** they model. A `Crate` in
the rule language mirrors `vx_rs::CrateGraph::Crate`. This is deliberate — the
type system is a projection of the host's domain model, not an independent
invention.

### Expressions

```
// Literals
42
"hello"
true
NodeKind::Action
[1, 2, 3]
{name: "foo", version: "1.0"}

// Let bindings
let graph = rust.crate_graph()
let root = rust.root(graph)

// Control flow
if crate.crate_type == CrateType::Bin {
    emit.node(Node {
        id: NodeId { value: str.cat("compile-bin:", crate.name) },
        kind: NodeKind::Action,
        label: str.cat("compile ", crate.name),
    })
}

// Iteration
for crate in closure {
    emit.node(Node {
        id: NodeId { value: str.cat("compile-rlib:", crate.name) },
        kind: NodeKind::Action,
        label: str.cat("compile ", crate.name),
    })
}

// Match (enum destructuring)
match crate.crate_type {
    CrateType::Bin => plan_bin(crate),
    CrateType::Lib => plan_lib(crate),
    _ => {},
}

// Field access
crate.name
crate.deps.len()

// Function definitions
fn plan_compile(crate: Crate, profile: String) {
    let node_id = NodeId { value: str.cat("compile:", crate.name) }
    emit.node(Node {
        id: node_id,
        kind: NodeKind::Action,
        label: str.cat("compile ", crate.name),
    })
}
```

### Builtin signature style

Builtins are namespaced and typed. No more stringly-typed map arguments.

```
// Graph emission — takes typed domain structs
emit.node(node: Node)
emit.edge(edge: Edge)
emit.fact(fact: Fact)

// Rust domain queries — return typed domain structs
rust.crate_graph() -> CrateGraph
rust.root(graph: CrateGraph) -> Crate
rust.select(graph: CrateGraph, name: String, crate_type: CrateType) -> List<Crate>
rust.transitive_closure(graph: CrateGraph, roots: List<Crate>) -> List<Crate>

// Environment (capability-gated)
env.profile() -> String
env.target_triple() -> String

// Transforms
str.cat(parts: List<String>) -> String   // or variadic: str.cat(a, b, c)
list.len<T>(list: List<T>) -> Int
list.map<T, U>(list: List<T>, f: fn(T) -> U) -> List<U>
list.filter<T>(list: List<T>, f: fn(T) -> Bool) -> List<T>
list.sort_by<T>(list: List<T>, f: fn(T) -> String) -> List<T>
map.keys<K, V>(map: Map<K, V>) -> List<K>
map.get<K, V>(map: Map<K, V>, key: K) -> Option<V>
```

Builtins use positional arguments for simple cases and named struct arguments
for complex cases. The type checker validates all call sites.

## Pipeline

```
                                    ┌──────────────┐
                                    │ .vx source   │
                                    └──────┬───────┘
                                           │
                                    ┌──────▼───────┐
                                    │    Lexer      │
                                    │  (tokens)     │
                                    └──────┬───────┘
                                           │
                                    ┌──────▼───────┐
                                    │    Parser     │
                                    │ (surface AST) │
                                    └──────┬───────┘
                                           │
                                    ┌──────▼───────┐
                                    │ Name resolver │
                                    │ + type checker│
                                    │  (typed IR)   │
                                    └──────┬───────┘
                                           │
                              ┌────────────┴────────────┐
                              │                         │
                       ┌──────▼───────┐          ┌──────▼───────┐
                       │ Interpreter  │          │ Kajit lower  │
                       │  (now)       │          │  (later)     │
                       └──────┬───────┘          └──────┬───────┘
                              │                         │
                              └────────────┬────────────┘
                                           │
                                    ┌──────▼───────┐
                                    │  RulePlan    │
                                    │  (IR)        │
                                    └──────┬───────┘
                                           │
                                    ┌──────▼───────┐
                                    │ ActionGraph  │
                                    │ (lowering)   │
                                    └──────────────┘
```

### Stage 1: Lexer (extend existing)

Add token kinds: `LAngle`, `RAngle`, `Arrow` (`->`), `DoubleColon` (`::`),
`Pipe` (`|`). Add keywords: `fn`, `struct`, `enum`, `match`, `let`, `if`,
`else`, `for`, `in`, `true`, `false`. Remove `Dollar`.

### Stage 2: Parser (rewrite)

Produces a surface AST with span information but no type annotations. Struct
literals, enum variant references (`NodeKind::Action`), function definitions,
match expressions, field access chains.

```rust
enum SurfaceExpr {
    Literal(Literal, Span),
    Ident(String, Span),
    FieldAccess(Box<SurfaceExpr>, String, Span),
    EnumVariant(String, String, Span),           // Type::Variant
    StructLiteral(String, Vec<(String, SurfaceExpr)>, Span),
    ListLiteral(Vec<SurfaceExpr>, Span),
    MapLiteral(Vec<(SurfaceExpr, SurfaceExpr)>, Span),
    Call(Box<SurfaceExpr>, Vec<SurfaceExpr>, Span),
    BinaryOp(BinOp, Box<SurfaceExpr>, Box<SurfaceExpr>, Span),
    If(Box<SurfaceExpr>, Block, Option<Block>, Span),
    For(String, Box<SurfaceExpr>, Block, Span),
    Match(Box<SurfaceExpr>, Vec<MatchArm>, Span),
    Let(String, Option<SurfaceType>, Box<SurfaceExpr>, Span),
    FnDef(String, Vec<Param>, Option<SurfaceType>, Block, Span),
    Block(Vec<SurfaceExpr>, Span),
}
```

### Stage 3: Name resolution + type checking (new)

Resolves identifiers against scope (locals, builtins, domain type
constructors). Checks that struct field names and types match. Checks that enum
variants exist. Checks that function call arguments match signatures. Produces a
typed IR where every expression has a resolved type.

```rust
struct TypedExpr {
    kind: TypedExprKind,
    ty: Type,
    span: Span,
}

enum Type {
    Bool,
    Int,
    String,
    List(Box<Type>),
    Map(Box<Type>, Box<Type>),
    Set(Box<Type>),
    Option(Box<Type>),
    Struct(StructId),
    Enum(EnumId),
    Fn(Vec<Type>, Box<Type>),
    Unit,
}
```

The type checker is a single forward pass — no inference, no unification. Types
are always known from declarations, literals, and builtin signatures. This keeps
the checker simple and the error messages direct.

### Stage 4a: Interpreter (rewrite, immediate)

A tree-walking interpreter over `TypedExpr`. Simpler than the current
`PlannerRuntime` because type checking already happened — no need to validate
argument shapes at runtime.

The interpreter still uses `PlannerHost` for side effects, but the host trait
methods receive typed arguments instead of `PlannerBuiltinArgs`. The capability
check moves to the type checker (calling `env.profile()` in a deterministic
context is a type error, not a runtime error).

### Stage 4b: Kajit lowering (later)

The typed IR lowers to Kajit's input representation. Since Kajit already handles
Cranelift codegen, this path JIT-compiles rule programs for production use. The
interpreter remains available for debugging and explain mode.

### Stage 5: RulePlan emission

Both the interpreter and Kajit produce `RulePlan` through the same `PlannerHost`
interface. The output is identical regardless of backend.

### Stage 6: ActionGraph lowering (existing)

`ActionGraph::from_rule_plan()` consumes the `RulePlan`. No changes needed.

## Top 5 Open Design Questions

### 1. Module system

How do rule files compose? Options:

- **Single file** (v0): one `.vx` file per project, no imports. Simplest, good
  enough for built-in rule packs.
- **Flat imports**: `use rust_rules` brings a rule pack's exports into scope.
  No nested modules.
- **Hierarchical modules**: full module tree with visibility. Probably
  over-engineering for v0.

Recommendation: start with single-file, design the import syntax now but don't
implement it until rule packs need composition.

### 2. User-defined types vs host-only types

Can rule authors define their own structs/enums, or are all types provided by
the host? Options:

- **Host-only types**: simpler, types always mirror Rust host structs. Rule
  authors work with what the host provides.
- **User-defined types**: more expressive, enables custom intermediate
  structures. Complicates the type checker and Kajit lowering.

Recommendation: host-only types for v0. User-defined types are a natural
extension but not needed until rule packs get complex enough to warrant local
abstractions.

### 3. Closures and higher-order functions

`list.map(crates, |c| c.name)` requires closures. Options:

- **No closures**: use `for` loops for everything. More verbose but simpler
  implementation.
- **Expression closures only**: `|params| expr`, no multi-statement bodies. Good
  enough for map/filter/sort_by.
- **Full closures**: `|params| { stmts }`. Full generality, more complex to
  type-check and lower to Kajit.

Recommendation: expression closures for v0. They cover the common cases (map,
filter, sort_by) without requiring a full closure implementation.

### 4. String interpolation vs str.cat

Building node IDs currently requires `str.cat("compile-bin:", crate.name)`.
Options:

- **Keep str.cat**: explicit, easy to implement, no parser complexity.
- **String interpolation**: `"compile-bin:{crate.name}"`. Nicer to read, parser
  needs to handle interpolation.

Recommendation: start with `str.cat`, add interpolation later. Interpolation is
syntactic sugar — it can be desugared to `str.cat` in the parser.

### 5. Error recovery and diagnostics strategy

The current parser stops at the first error. For LSP and CI, we need better
error recovery. Options:

- **Synchronization-point recovery**: skip to next semicolon/brace on error,
  continue parsing. Standard approach.
- **Error node AST**: parse errors become nodes in the AST, type checker skips
  them. Better for LSP partial analysis.
- **Defer to later**: keep fail-fast for now, add recovery when LSP work starts
  (#229).

Recommendation: fail-fast for the interpreter path, but design the AST with
`Option<Span>` and error node support so recovery can be added without
restructuring.

---

## Appendix: Comparison with Current vx-plan Prototype

| Aspect | Current prototype | Proposed design |
|---|---|---|
| Value system | `PlannerValue` enum (dynamic) | Typed IR with static checking |
| Variables | `$var` dollar-sign references | Plain identifiers |
| Builtin args | String-keyed maps, runtime parsing | Typed parameters, static checking |
| Enum variants | Keywords (`action`, `ifchanged`) | Typed variants (`NodeKind::Action`) |
| Struct construction | Map literals `{id: n1, kind: action}` | Typed struct literals `Node { id: ..., kind: ... }` |
| Function defs | None | `fn name(params) -> ReturnType { body }` |
| Match | None | `match expr { Pattern => expr }` |
| Error detection | Runtime (wrong type passed to builtin) | Type checker (before execution) |
| Generic types | None | `List<T>`, `Map<K, V>`, `Option<T>` |
| Parser output | `PlannerExpr` (runtime AST) | `SurfaceExpr` -> `TypedExpr` (two stages) |
| Interpreter | `PlannerRuntime<H: PlannerHost>` | Same pattern, typed host interface |
| JIT path | None | Kajit lowering from typed IR (later) |
| RulePlan output | Same | Same |
| ActionGraph lowering | Same | Same |

The key insight: **everything below the typed IR stays the same**. RulePlan,
ActionGraph, the executor, the parity harness, the lowering bridge — none of
that changes. The rewrite is above the typed IR: a new frontend that produces
better input to the same backend.
