# 010-0: Core Non-Capturing Lowering

Status: Not started.

## Context

Work items 000–009 are done. VIR has name resolution, type checking, capture analysis, demand analysis, and lazy field seeding. The next step is lowering VIR to KernelOp bytecode — the SSA-like instruction set interpreted by `vx-rt`.

The current lowering lives in `crates/vx-analysis/src/exec_slice.rs` (~3150 lines). It consumes the bridge's flat span-based data (`AnalysisResult` + `SingleSourceBridge`) and produces `ExecutableProgram`. We are building a replacement that consumes `VirProgram` + `VirDemandResult` and produces the **same** `ExecutableProgram` output type, so `vx-rt` can evaluate it unchanged.

010 is split into three phases:

- **010-0** (this item): Core non-capturing lowering — literals, bindings, arithmetic, control flow, direct function calls. No closures, no products, no laziness, no runtime boundaries.
- **010-1**: Closures, products, field projection, lazy thunks, capture threading.
- **010-2**: Runtime boundaries (exec/discover/shared-keyed), Where, Invoke, and remaining exotic constructs.

Each phase compiles with explicit `UnsupportedShape(...)` for not-yet-covered variants and produces a runnable subset with dedicated eval tests.

## Output types

The lowering produces `vx_analysis::ExecutableProgram`, reusing the existing types from `vx-analysis`:

```rust
// crates/vx-analysis/src/exec_slice.rs
pub struct ExecutableProgram {
    pub entry: BodyId,
    pub bodies: BTreeMap<BodyId, ExecutableBody>,
    pub runtime_nodes: BTreeMap<RuntimeNodeId, ExecutableRuntimeNode>,
}

pub struct ExecutableBody {
    pub id: BodyId,
    pub kernel: KernelId,
    pub kernel_kind: KernelKind,
    pub params: Vec<SlotId>,
    pub ops: Vec<KernelOp>,
    pub result_source: ResultSource,
    pub runtime_boundary_extractions: Vec<RuntimeNodeId>,
    pub local_call_dependencies: Vec<BodyId>,
}
```

These use `vx_analysis::{BodyId, ExprId, SlotId, KernelId, RuntimeNodeId, SourceExpr}`. Since both VIR and analysis IDs are `u32` newtypes assigned sequentially from 0, we convert via raw `.0` field access: `vx_analysis::BodyId(vir_body_id.0)`.

**Dependency**: `vx-vir` gains a dependency on `vx-analysis` for the output types only. No circularity — `vx-analysis` does not depend on `vx-vir`. (In 011-013 we will extract these shared types into a common crate and remove the dependency.)

## ID mapping

VIR allocates BodyId, ExprId sequentially via `VirArena::alloc_body`, `alloc_expr`. The analysis pipeline allocates its own BodyId, ExprId in `SingleSourceBridge`. These numbering schemes are **independent** — they will not match for the same source file.

This is fine because the lowering produces a self-contained `ExecutableProgram` where all internal ID references (CallBody callee, AllocClosure/AllocThunk body, SourceExpr) are consistent with each other. The runtime only needs internal consistency, not agreement with the bridge's IDs.

Conversion helper:

```rust
fn analysis_body_id(id: crate::BodyId) -> vx_analysis::BodyId {
    vx_analysis::BodyId(id.0)
}
fn analysis_expr_id(id: crate::ExprId) -> vx_analysis::ExprId {
    vx_analysis::ExprId(id.0)
}
fn source_expr(body: crate::BodyId, expr: crate::ExprId) -> vx_analysis::SourceExpr {
    vx_analysis::SourceExpr {
        body: analysis_body_id(body),
        expr: analysis_expr_id(expr),
    }
}
```

## Architecture

New file: `crates/vx-vir/src/lower.rs` (add `pub mod lower;` to `lib.rs`).

### Entry point

```rust
pub fn lower_vir_program(
    program: &VirProgram,
    demand: &VirDemandResult,
) -> Result<ExecutableProgram, LowerError>
```

1. Pick entry point from `demand.entry_points`. If empty, return `LowerError::NoEntryPoint` — this happens when source contains only top-level expressions with no named functions (008 explicitly allows empty entry_points).
2. Iterate `demand.reachable` — for each reachable body, create `VirBodyLowering` and lower it.
3. Collect all `ExecutableBody` results (including synthesized thunk/closure bodies from 010-1) into the `BTreeMap`.
4. Return `ExecutableProgram { entry, bodies, runtime_nodes: BTreeMap::new() }`.

### VirBodyLowering

Per-body lowering context. Analogous to exec_slice's `BodyLowering` but taking VIR data:

```rust
struct VirBodyLowering<'a> {
    arena: &'a VirArena,
    body_id: crate::BodyId,

    // Slot management
    next_slot: u32,
    slot_by_binding: HashMap<crate::BindingId, SlotId>,
    materialized: HashSet<crate::BindingId>,

    // Output
    ops: Vec<KernelOp>,
    params: Vec<SlotId>,
    local_call_deps: BTreeSet<vx_analysis::BodyId>,
}
```

**Slot allocation**: On construction, pre-allocate param slots. Parameters are taken from `arena[body_id].params` — iterate in order, extract the `BindingId` from each param's pattern (simple binding only; destructuring params return `UnsupportedShape` in 010-0). Each param gets a slot starting from 0.

Let bindings get slots allocated lazily during `materialize_binding`.

### Operand model

Same as exec_slice: `lower_expr` returns `vx_analysis::Operand` (Slot, Int, or String). This allows inline constants to skip slot allocation.

```rust
fn lower_expr(&mut self, expr_id: crate::ExprId) -> Result<Operand, LowerError>
```

`ensure_slot(operand) -> SlotId`: if operand is already a Slot, return it. If Int or String, emit `AssignConstInt`/`AssignConstString`, return the new slot.

### Binding materialization

When a `NameRef` resolves to a binding:

1. If already materialized, return `Operand::Slot(slot)`.
2. If it's a parameter (slot already allocated), mark materialized, return slot.
3. If it has `init_expr`: recursively `lower_expr(init_expr)`. If the result is the binding's own slot (self-referential alias), skip. Otherwise emit `AssignAlias` to copy into the binding's slot. Mark materialized.

This is dramatically simpler than exec_slice's version because VIR gives us `Binding.init_expr` directly — no span-based `binding_initializers` map, no `use_binding_by_expr` lookups.

## Expression lowering (010-0 scope)

### Literal

```
LitKind::Int { raw } → parse raw as i64 → Operand::Int(value)
LitKind::String(s) → Operand::String(s)
LitKind::Bool(true/false) → emit AssignConstBool, return Slot
LitKind::Float → UnsupportedShape (no runtime support yet)
```

### NameRef

```
resolved = Some(DefTarget::Binding(bid)) → materialize_binding(bid)
resolved = Some(DefTarget::Item(item_id)) where item is Function →
    UnsupportedShape in 010-0 (this is "function used as value" = closure allocation, deferred to 010-1)
resolved = Some(DefTarget::Variant { .. }) → UnsupportedShape
resolved = None → LowerError (unresolved name)
```

### Block

```
ExprKind::Block { stmts, tail } →
    for each stmt: lower_stmt(stmt)
    if tail: lower_expr(tail)
    else: UnsupportedShape (unit-returning blocks)
```

### Let statement

```
StmtKind::Let { pattern, init, .. } →
    // Don't lower init eagerly — it will be materialized when the binding is used.
    // But we DO need to allocate a slot for the binding.
    extract BindingId from pattern (simple binding only)
    allocate slot, register in slot_by_binding
    // The init will be lowered during materialize_binding when the name is referenced.
```

**Wait — this is wrong for side-effecting inits.** If the init has side effects (e.g., `let _ = trap("boom")`), we must lower it eagerly. But in the current runtime model, KernelOp evaluation is demand-driven (the runtime only evaluates ops whose slots are needed). So the current exec_slice also materializes lazily. We follow the same approach.

Actually, re-reading exec_slice: `materialize_binding` IS called lazily when a use references the binding. If a binding is never used, its initializer is never lowered. This matches the demand-driven semantics.

### Expr statement

```
StmtKind::Expr(expr_id) → lower_expr(expr_id)
    // Result discarded (side effects only). In a demand-driven model,
    // unreferenced intermediate values won't be evaluated anyway.
    // But we still need to lower for the case where the result slot
    // IS the body's result.
```

### Assign statement

```
StmtKind::Assign { value, resolved: Some(bid), .. } →
    lower value, emit AssignAlias to binding's slot
    (mutation semantics — re-bind the slot)
```

### Return statement

```
StmtKind::Return { value } → lower value, mark as result
```

### Binary

```
BinOp::Add → emit Add
BinOp::Sub → emit Sub
BinOp::Eq → emit Equal
BinOp::Neq → emit NotEqual
BinOp::Concat → emit ConcatStrings with [lhs, rhs]
Others (Mul, Div, Rem, Lt, LtEq, Gt, GtEq, And, Or, Pipe) → UnsupportedShape
```

Only Add, Sub, Eq, Neq are supported by the runtime currently. Concat maps to ConcatStrings.

### Prefix

```
PrefixOp::Not → UnsupportedShape (no KernelOp for boolean negation yet)
PrefixOp::Neg → UnsupportedShape (no signed integers)
```

### If

```
ExprKind::If { condition, then_branch, else_branch } →
    lower all three, emit Select { condition, then_value, else_value }
    if no else_branch → UnsupportedShape (no unit values)
```

### Fail

```
ExprKind::Fail { message } →
    lower message (or use default), emit AssignFailure
```

### InterpolatedString

```
ExprKind::InterpolatedString { parts } →
    collect Vec<Operand> from parts (text → Operand::String, expr → lower_expr)
    emit ConcatStrings { parts }
```

### Call (direct named-body only)

```
ExprKind::Call { callee, args } →
    if callee is NameRef → DefTarget::Item(item_id) → Function →
        get function's BodyId
        lower args
        emit CallBody { callee: body_id, args }
        add to local_call_deps
    else →
        UnsupportedShape in 010-0 (closure application, method calls, etc.)
```

### Everything else → UnsupportedShape

Closure, Tuple, Array, Record, Map, StructLiteral, Where, Invoke, PartialApp, Index, Field, Match, Range, TypeOp, With, Fallible, Import, PathLiteral, Error — all return `UnsupportedShape("...")` in 010-0.

Similarly for StmtKind: CompoundUpdate, FieldAssign, Item → UnsupportedShape.

## Error type

```rust
#[derive(Debug, Clone)]
pub enum LowerError {
    UnsupportedShape(String),
    UnresolvedName { name: String, expr: crate::ExprId },
    NoEntryPoint,
    MissingBody(crate::BodyId),
    InvalidLiteral { raw: String },
}
```

## Files to modify

| File | Change |
|------|--------|
| `crates/vx-vir/Cargo.toml` | Add `vx-analysis` dependency |
| `crates/vx-vir/src/lib.rs` | Add `pub mod lower;` |
| `crates/vx-vir/src/lower.rs` | New: ~500 lines, core lowering |
| `crates/vx-vir/tests/lower.rs` | New: eval tests via vx-rt |

## Testing

Tests go in `crates/vx-vir/tests/lower.rs`. Each test builds a VIR program from source, runs demand analysis, lowers to `ExecutableProgram`, and evaluates via `vx_rt::eval_program`.

Helper:

```rust
fn eval_vir(source: &str, args: &[RtValue]) -> Result<RtValue, ...> {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    resolve_names(&mut program.arena);
    let demand = analyze_demand(&program);
    let executable = lower_vir_program(&program, &demand)?;
    eval_program(&executable, args)
}
```

Test cases:

1. **Constant return**: `fn main() -> U64 ( 42 )` → 42
2. **Param passthrough**: `fn main(n: U64) -> U64 ( n )` → args[0]
3. **Addition**: `fn main() -> U64 ( 20 + 22 )` → 42
4. **Subtraction**: `fn main(n: U64) -> U64 ( n - 1 )` → args[0] - 1
5. **Let binding**: `fn main() -> U64 ( let x = 40; x + 2 )` → 42
6. **Chained lets**: `fn main() -> U64 ( let a = 10; let b = a + 20; b + 12 )` → 42
7. **If/select true**: `fn main() -> U64 ( if true ( 42 ) else ( 0 ) )` → 42
8. **If/select false**: `fn main() -> U64 ( if false ( 0 ) else ( 42 ) )` → 42
9. **Equality**: `fn main() -> U64 ( if 1 == 1 ( 42 ) else ( 0 ) )` → 42
10. **Not-equal**: `fn main() -> U64 ( if 1 != 2 ( 42 ) else ( 0 ) )` → 42
11. **Direct call**: `fn add_one(n: U64) -> U64 ( n + 1 ); fn main() -> U64 ( add_one(41) )` → 42
12. **Call chain**: `fn f(n: U64) -> U64 ( n + 1 ); fn g(n: U64) -> U64 ( f(n) + 1 ); fn main() -> U64 ( g(40) )` → 42
13. **Fail**: `fn main() -> U64 ( fail("boom") )` → RtError::Failure("boom")
14. **String interpolation**: Source produces a string (test with ConcatStrings)
15. **Unused binding**: `fn main() -> U64 ( let x = 99; 42 )` → 42 (x never materialized)
16. **Bool literal**: `fn main() -> U64 ( if true ( 42 ) else ( 0 ) )` → 42

## Verification

1. `cargo check -p vx-vir`
2. `cargo nextest run -p vx-vir --test lower`
3. `cargo nextest run` — only pre-existing failures
