# 015: Product-pattern match lowering

Status: Not started.

## Goal

Lower match patterns for runtime shapes that already exist: tuples and structs/records.

## Depends on

010-1.

## Scope

- **Tuple destructuring patterns**: `match p ( (x, y) => x + y )`
- **Struct/record destructuring patterns**: `match s ( { a, b } => a + b )`, `match s ( Pair { a, b } => a )`
- **Wildcard and binding patterns in nested contexts**: `match p ( (_, y) => y )`
- **Nested product patterns**: `match p ( ((a, _), _) => a )`

### Lowering strategy

Product patterns lower to a sequence of projections + temporary bindings, wrapped in the existing `Select` chain machinery from 010-2's match lowering.

For `match scrutinee ( (x, y) => body )`:
1. Lower scrutinee → `scrutinee_op`
2. Emit `ProjectIndex { pair: scrutinee_op, index: 0 }` → `x_slot`
3. Emit `ProjectIndex { pair: scrutinee_op, index: 1 }` → `y_slot`
4. Register `x` and `y` bindings in `slot_by_binding`, mark materialized
5. Lower `body` with bindings available

For struct patterns `match s ( { a, b } => body )`:
1. Lower scrutinee
2. Emit `ProjectNamedField { value: scrutinee_op, field: "a" }` → `a_slot`
3. Emit `ProjectNamedField { value: scrutinee_op, field: "b" }` → `b_slot`
4. Register bindings, lower body

Nested patterns recurse: each sub-pattern receives the projected operand as its "scrutinee."

### What's NOT in scope

- Variant patterns — requires runtime discriminant support (016)
- Array patterns — no array indexing runtime ops exist yet
- Or-patterns — separate complexity (017)
- Guard clauses — separate complexity (017)

## Files to modify

| File | Change |
|------|--------|
| `crates/vx-vir/src/lower.rs` | Extend `lower_match` to handle `PatternKind::Tuple`, `PatternKind::Struct` |
| `crates/vx-vir/tests/lower.rs` | Add eval tests |

## Testing

1. `match (1, 2) ( (x, y) => x + y )` → 3
2. `match { a: 42, b: 99 } ( { a } => a )` → 42
3. `match ((1, 2), 3) ( ((x, _), _) => x )` → 1
4. `match p ( (_, y) => y )` where `p = (99, 42)` → 42
5. Struct pattern with unused fields: `match s ( { a } => a )` where `s` has fields `a`, `b`
6. Binding + wildcard combinations in tuple positions

## Verification

1. `cargo check -p vx-vir`
2. `cargo nextest run -p vx-vir --test lower`
3. `cargo nextest run` — only pre-existing failures
