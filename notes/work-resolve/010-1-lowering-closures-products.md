# 010-1: Closures, Products, and Lazy Thunks

Status: Not started.

## Context

010-0 delivers core non-capturing lowering: literals, bindings, arithmetic, control flow, and direct named-body calls. 010-1 adds the product/closure/laziness layer.

This is where the VIR advantages over the bridge really pay off:

- **Captures are explicit**: `Body.captures: Vec<BindingId>` from capture analysis (007) gives us the exact set of captured bindings. No "capture everything then prune" strategy needed.
- **Lazy fields are explicit**: `VirDemandResult.lazy_fields: Vec<LazyField>` from lazy field seeding (009) tells us exactly which product fields to thunk.
- **Static field resolution is simple**: `NameRef.resolved → Binding.init_expr` gives us direct binding-to-initializer chains. No span-based `products`/`projections` bridge tables.

## Depends on

010-0 (core lowering infrastructure).

## Scope

### Closures

**Closure expression** (`ExprKind::Closure`):

1. The closure's body is `ClosureExpr.body` (a `BodyId` in the VIR arena).
2. `arena[closure_body].captures` gives the captured `BindingId`s — already computed by capture analysis.
3. For each capture: look up the parent's `slot_by_binding[capture_bid]` → that's the capture operand.
4. `arena[closure_body].params` gives the parameter count (filter `!is_keyword`).
5. Emit `AllocClosure { body, captures, param_count }`.

The closure body must itself be lowered as an `ExecutableBody` where:
- Capture slots come first: `0..captures.len()`
- Parameter slots follow: `captures.len()..captures.len() + params.len()`
- The body's root expression is lowered in this child context.

**Closure application** (`ApplyClosure`):

`ApplyClosure` is emitted when `ExprKind::Call { callee, args }` and the callee resolves to a **binding** (not a named function item). The rule:

- Callee is `NameRef → DefTarget::Binding(bid)`: the binding holds a closure value at runtime (whether from a closure expression, a function-as-value, or a function-typed parameter). Emit `ApplyClosure { closure: materialize(bid), args }`.
- Callee is `NameRef → DefTarget::Item(Function)`: this is a direct known call → `CallBody` (handled in 010-0, unchanged).

There's no third case for Call. If callee is a complex expression (e.g., `(make_fn())(arg)`) the callee is lowered to an operand and `ApplyClosure` is emitted.

**Function-as-value rule** (crisp):

- **Direct known call position** (callee of `ExprKind::Call` where callee is `NameRef → DefTarget::Item(Function)`): emit `CallBody`. Already handled in 010-0.
- **Non-call position** (NameRef → Function item appearing as argument, let initializer, tuple element, etc.): emit `AllocClosure { body: fn_body, captures: [], param_count }`. This wraps the named function as a closure value. This was UnsupportedShape in 010-0.

There is no ambiguity: the dispatch point is whether the NameRef is the immediate callee of a Call expression. If yes → `CallBody`. If no → `AllocClosure`.

**Capture pruning and slot remapping**:

In the current exec_slice, thunk/closure bodies go through a prune step: walk all ops to find referenced capture slots, remove unused ones, remap all remaining slots. With VIR's explicit captures from 007, the capture list should already be minimal. However, thunk bodies (see below) may still need pruning because a thunk wraps a single field expression that may not reference all parent bindings. We keep the prune-and-remap utilities:

```rust
fn collect_referenced_slots(ops: &[KernelOp], result: SlotId) -> HashSet<SlotId>
fn remap_op_slots(ops: &mut [KernelOp], map: &HashMap<SlotId, SlotId>)
```

### Products

**Tuple** (`ExprKind::Tuple`):

Only 2-element tuples are supported — the current exec_slice explicitly rejects other arities with `UnsupportedShape("only 2-item tuples supported")`. We do the same. The runtime's `RtValue::Pair` is a strict 2-pair, not nested.

For a tuple `(a, b)`:
- Check `lazy_fields` for this expression: if `Index(0)` is lazy, synthesize a thunk body for `a` instead of lowering it directly. Same for `Index(1)`.
- Lower non-lazy elements directly.
- Emit `MakePair { first, second }`.

Tuples with 0, 1, or 3+ elements → `UnsupportedShape`.

**Record** (`ExprKind::Record`):

Emit `MakeStruct { fields }`. Each field may be lazy → thunk.

**StructLiteral** (`ExprKind::StructLiteral`):

Same as Record: emit `MakeStruct { fields }`. Lazy field check uses `FieldKey::Name(field_name)`.

**Array** (`ExprKind::Array`):

Emit `MakeArray { items }`. No laziness for arrays.

**Map** (`ExprKind::Map`):

UnsupportedShape in 010-1 (or simple lowering if straightforward). Maps are uncommon in the executable slice.

### Field projection

**Field access** (`ExprKind::Field`):

This is where static resolution makes VIR shine. For `p._0`:

1. **Try static resolution**: Call `resolve_to_product(arena, field.receiver)` (reuse from demand.rs or similar logic). If it resolves to a tuple/record/struct construction, find the sub-expression at the projected field. Lower that sub-expression directly. No runtime ProjectIndex needed.

2. **Fall back to runtime**: If static resolution fails, emit `ProjectIndex` (for `FieldKey::Index`) or `ProjectNamedField` (for `FieldKey::Name`).

Static resolution through binding chains:
```
p._0 where p = (a, b) → lower a directly
q.name where q = { name: expr, ... } → lower expr directly
```

This uses the same `resolve_to_product` logic from demand.rs:
- NameRef → Binding → init_expr → recurse
- Field → resolve receiver, index into product, recurse on sub-expression

**Index access** (`ExprKind::Index`):

For array indexing. UnsupportedShape or simple lowering depending on runtime support.

### Lazy thunks

When `VirDemandResult.lazy_fields` marks a field as lazy, the field's expression must be wrapped in a thunk body instead of being lowered inline.

**`synthesize_thunk_body(expr_id, parent_lowering) → (BodyId, Vec<Operand>)`**:

Strategy: **provision all parent bindings as captures, then prune unused ones.** This matches the current exec_slice approach and is simpler than pre-computing referenced bindings (which would require a separate expression walk). The pruning step is cheap and already needed for correctness.

1. Allocate a new `BodyId` for the thunk. Use a global counter on the lowering context (NOT the per-body 10000-based hack from exec_slice), initialized to `max(VIR body count) + 1`.
2. Collect all parent `slot_by_binding` entries → these are the provisional captures.
3. Create a child `VirBodyLowering` with:
   - Capture slots: `0..N` for each provisioned parent binding (one thunk param per potential capture)
   - No additional param slots (thunks take no arguments beyond captures)
4. Lower the thunk expression in the child context.
5. **Prune**: Walk child ops + result slot to find referenced capture slots. Remove unused captures. Remap all remaining slots to compact indices.
6. **Materialize used captures in parent**: For each kept capture, call `parent.materialize_binding(bid)` to ensure the parent has the value in its slot (the capture operand references this slot).
7. Return `(thunk_body_id, pruned_captures)`.

The parent emits `AllocThunk { thunk_body, captures }` in place of the direct lowering.

**Thunk body ID allocation**:

Add a `next_body_id: u32` to the top-level lowering context (`LoweringContext`), initialized to `max(existing VIR body IDs) + 1`. Each synthesized thunk/closure body increments this counter. This gives globally unique BodyIds without the `10000` hack.

### Match expressions

Match should be lowered in 010-1 since it depends on product/pattern handling. A match on a simple scalar (int/bool) can be lowered as nested `Select` ops. A match with destructuring patterns requires product projection.

Simple approach for 010-1:
- Match on bool/int literals → chain of Select ops
- Match with wildcard/binding patterns → handle naturally
- Match with tuple/struct destructuring → project fields, bind results
- Match with variant patterns → UnsupportedShape (deferred)

## Files to modify

| File | Change |
|------|--------|
| `crates/vx-vir/src/lower.rs` | Extend: closures, products, field projection, thunks (~800 lines) |
| `crates/vx-vir/tests/lower.rs` | Add eval tests for new features |

## Testing

Extend `crates/vx-vir/tests/lower.rs` with eval tests:

1. **Simple tuple projection**: `fn main() -> U64 ( let p = (42, 99); p._0 )` → 42
2. **Tuple second field**: `fn main() -> U64 ( let p = (99, 42); p._1 )` → 42
3. **Lazy field (bomb not evaluated)**: `fn bomb() -> U64 ( bomb() ); fn main() -> U64 ( let p = (42, bomb()); p._0 )` → 42
4. **Record field**: `fn main() -> U64 ( let r = { a: 42, b: 99 }; r.a )` → 42
5. **Struct literal field**: `struct Pair { a: U64, b: U64 }; fn main() -> U64 ( let p = Pair { a: 42, b: 99 }; p.a )` → 42
6. **Closure capture**: `fn main(n: U64) -> U64 ( let add_n = (x) => x + n; add_n(32) )` → args[0] + 32
7. **Closure no capture**: `fn main() -> U64 ( let f = (x) => x + 1; f(41) )` → 42
8. **Higher-order function**: `fn apply(f: Fn(U64) -> U64, x: U64) -> U64 ( f(x) ); fn main() -> U64 ( let inc = (x) => x + 1; apply(inc, 41) )` → 42
9. **Function as value**: `fn add_one(n: U64) -> U64 ( n + 1 ); fn apply(f: Fn(U64) -> U64, x: U64) -> U64 ( f(x) ); fn main() -> U64 ( apply(add_one, 41) )` → 42
10. **Chained projection**: `fn main() -> U64 ( let p = ((42, 99), 0); p._0._0 )` → 42
11. **Deep lazy chain**: `fn bomb() -> U64 ( trap("boom") ); fn main() -> U64 ( let p = ((7, bomb()), (bomb(), bomb())); p._0._0 )` → 7
12. **Struct lazy field**: `struct Pair { a: U64, b: U64 }; fn bomb() -> U64 ( trap("boom") ); fn main() -> U64 ( let s = Pair { a: 42, b: bomb() }; s.a )` → 42
13. **Cross-function lazy**: `fn pick(p: (U64, U64)) -> U64 ( p._0 ); fn bomb() -> U64 ( trap("boom") ); fn main() -> U64 ( pick((42, bomb())) )` → 42
14. **Closure with lazy tuple**: `fn main() -> U64 ( let f = (p) => p._0; f((42, trap("boom"))) )` → 42
15. **Array construction**: `fn main() -> U64 ( 42 )` with array test (if MakeArray has runtime support)
16. **Binding chain through product**: `fn main() -> U64 ( let p = (1, 2); let q = p; q._0 )` → 1
17. **Match on bool**: `fn main() -> U64 ( let x = true; match x ( true => 42, false => 0 ) )` → 42

## Verification

1. `cargo check -p vx-vir`
2. `cargo nextest run -p vx-vir --test lower`
3. `cargo nextest run` — only pre-existing failures
