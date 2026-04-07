# 006: Extend KernelOps for thunks

Replace `AssignLazyPoison` with real thunk ops so `ExecutableProgram` preserves
lazy field computation code instead of discarding it.

## Current state

When lowering a tuple where a field is lazy (`lazy_fields_by_expr`), the lowerer
at `exec_slice.rs:1496-1510` attempts `lower_expr(item_id)`, catches
`UnsupportedShape`, and emits `AssignLazyPoison` — discarding the computation
code entirely.

## What needs to change

### New KernelOp variant

```rust
AllocThunk {
    dst: SlotId,
    source: SourceExpr,
    thunk_body: BodyId,
    captures: Vec<Operand>,  // parent scope bindings the thunk needs
}
```

### New ExecutableBody variant

Thunk bodies are stored as regular `ExecutableBody` entries in
`ExecutableProgram.bodies`. Add a `BodyKind` enum to distinguish:

```rust
enum BodyKind {
    Regular,
    Thunk { parent_expr: SourceExpr },
}
```

### Pattern to follow

Closure shipping (`exec_slice.rs:1174-1228`) already does something similar:
creates a child `BodyLowering` that clones parent binding context and lowers
the closure body into a separate op sequence. Thunk body synthesis should
follow the same pattern.

## Key implementation detail

When synthesizing a thunk body:
1. Walk the lazy field's AST to find referenced bindings
2. Create synthetic params for each captured binding
3. Lower the field expression in a child `BodyLowering` with those params
4. Store as new `ExecutableBody` with `BodyKind::Thunk`
5. Emit `AllocThunk` in the parent with capture operands

## Key files

- `crates/vx-analysis/src/exec_slice.rs` — KernelOp enum (line 122),
  tuple lowering (line 1489), shipped closure pattern (line 1174)
- `crates/vx-analysis/src/model.rs` — ExtractedNodeReason::LazyProductField
- `crates/vx-analysis/src/bridge_to_analysis.rs` — seed_lazy_product_fields

## Status

Not started.
