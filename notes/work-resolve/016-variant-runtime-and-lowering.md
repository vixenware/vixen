# 016: Variant runtime representation + variant lowering

Status: Not started.

## Goal

Make enums/variants first-class at runtime so variant construction and variant matching are possible.

## Depends on

010-1, probably 015's control-flow machinery.

## Context

Currently `RtValue::Struct(Vec<(String, Thunk)>)` is used for both structs and variants. There's no discriminant — no way to tell `Some { val: 42 }` from `None {}` at runtime. The AST-walking evaluator (vx-eval-tests) has a separate `Value::Variant` with an `enum_name` + `variant` tag, but the bytecode runtime has nothing.

The existing `KernelOp` set has no variant construction or discriminant check ops.

## Scope

### Runtime changes (vx-rt)

Extend `RtValue` with a tagged variant representation:
```rust
RtValue::Variant {
    tag: String,           // variant name, e.g. "Some", "None"
    fields: Vec<(String, Thunk)>,  // payload fields
}
```

Alternative: `RtValue::Variant(String, Vec<(String, Thunk)>)` — tag + fields.

### KernelOp additions (vx-analysis)

Add new ops or repurpose existing ones:

- **MakeVariant**: `{ dst, source, tag: String, fields: Vec<(String, Operand)> }` — construct a tagged variant value. (Could reuse `MakeStruct` with an extra tag field, but a separate op is clearer.)
- **CheckTag**: `{ dst, source, value: Operand, tag: String }` → `Bool` — check if a variant's tag matches. Used in match lowering.
- **ProjectVariantField**: reuse `ProjectNamedField` — once we know the tag matches, field projection is the same.

### Lowering changes (vx-vir)

**Variant construction** (`ExprKind::StructLiteral` where the name resolves to a variant):
1. Detect that the struct literal's `resolved` item is an enum (check `ItemKind::Enum`)
2. Emit `MakeVariant { tag: variant_name, fields }` instead of `MakeStruct`

**Variant match patterns** (`PatternKind::Variant`):
1. Emit `CheckTag { value: scrutinee, tag: variant_name }` → condition
2. If tag matches, project payload fields with `ProjectNamedField`
3. Bind projected fields to pattern bindings
4. Chain with `Select` as in existing match lowering

### Bare variant references

`NameRef` resolving to `DefTarget::Variant { enum_item, variant_index }`:
- If in call position (struct literal syntax): handled by StructLiteral lowering
- If as value (e.g. `None` without braces): emit `MakeVariant { tag: "None", fields: [] }`

## Why separate

This is not "just lowering." It changes the runtime representation. It deserves its own item.

## Files to modify

| File | Change |
|------|--------|
| `crates/vx-analysis/src/exec_slice.rs` | Add `MakeVariant`, `CheckTag` to `KernelOp` enum |
| `crates/vx-rt/src/lib.rs` | Add `RtValue::Variant`, implement `MakeVariant`/`CheckTag` eval |
| `crates/vx-vir/src/lower.rs` | Variant construction, variant pattern matching |
| `crates/vx-vir/tests/lower.rs` | Eval tests |

## Testing

1. Construct and match simple variant: `Some { val: 42 }` → match → extract val
2. Zero-payload variant: `None {}` → match → return default
3. Payload-bearing variant with multiple fields
4. Nested variant match: `Some { val: Some { val: 42 } }`
5. Multi-arm match with different variants: `Some => ..., None => ...`
6. Variant with lazy field: `Some { val: 42, extra: bomb() }` → only project `val`

## Verification

1. `cargo check -p vx-analysis -p vx-rt -p vx-vir`
2. `cargo nextest run -p vx-vir --test lower`
3. `cargo nextest run -p vx-rt`
4. `cargo nextest run` — only pre-existing failures
