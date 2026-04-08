# 010-2: Runtime Boundaries and Exotic Constructs

Status: Not started.

## Context

010-0 delivers core non-capturing lowering. 010-1 adds closures, products, field projection, lazy thunks, and basic match. 010-2 completes the lowering pass with runtime boundaries and the remaining expression kinds.

## Depends on

010-0, 010-1.

## Scope

**Lowered in 010-2**: Runtime boundaries (exec/discover/shared_keyed + Where-with-runtime), Invoke, PathLiteral, TypeOp::Ascription (passthrough), builtin method calls (.len(), .slice(), .collect(), .blob(), .which()), remaining Match patterns (variants, arrays, or-patterns).

**Explicitly UnsupportedShape in 010-2**: Non-runtime Where, PartialApp, With, Map, Range, Fallible, Import, TypeOp::As, TypeOp::Is.

### Runtime boundaries

Runtime boundaries are calls that cross the Vixen/host boundary: `exec`, `discover`, `shared_keyed`. They produce `AwaitRuntime` ops and `ExecutableRuntimeNode` entries in the output.

In the current pipeline, runtime boundary information comes from `AnalysisResult.runtime_nodes` and `AnalyzedBody.extracted_nodes`. In the VIR pipeline, we need a different approach since `VirDemandResult` doesn't currently model runtime boundaries — it only has call graph edges and lazy fields.

**Options**:

A. Add runtime boundary detection to VIR demand analysis (extend 008). The VIR builder or a post-pass identifies calls to builtin runtime functions (exec, discover, shared_keyed) and records them.

B. Detect runtime boundaries during lowering itself. When lowering a Call expression, check if the callee resolves to a known runtime boundary function. If so, emit `AwaitRuntime` instead of `CallBody`.

Option B is simpler for now and matches how exec_slice handles it (the boundary detection is inline in `lower_expr`).

**Runtime boundary detection heuristic**: In the current bridge, runtime calls are identified by their resolved DefId pointing to specific built-in function names (`exec`, `discover`, `shared_keyed`). In VIR, a `NameRef` resolving to `DefTarget::Item(item_id)` where the item's DefId is a known runtime function.

**AwaitRuntime lowering**:

```
ExprKind::Call { callee, args } where callee is a runtime boundary →
    allocate RuntimeNodeId
    build RuntimeBoundaryInputSpec for each arg:
        - literal args → IntLiteral / StringLiteral / PathLiteral
        - binding args → Binding { slot }
        - complex args → lower_expr, ensure_slot, Binding { slot }
    emit AwaitRuntime { node, positional_inputs, mounts: [], writable_roots: [], coarse_mounts: [] }
    register ExecutableRuntimeNode in output
```

**Where + runtime boundary**:

`WhereExpr { callee: Call(exec(...)), keywords }` — the Where clause provides mount specifications, writable roots, observer closures. Lower the inner call to AwaitRuntime and attach the Where-provided data.

### Where expression

`ExprKind::Where { callee, keywords }`:

- If callee is a runtime boundary call (exec/discover/shared_keyed) or wraps one (e.g., `exec(...) where { mounts: ... }`): lower as `AwaitRuntime` with the Where-provided fields (mounts, writable_roots, observer closure) attached. This is the primary use case for Where in the executable slice.
- **Non-runtime Where** (`f(x) where { key: val }`): `UnsupportedShape` in 010-2. Non-runtime Where is keyword argument passing, which the current exec_slice also doesn't fully support in the executable slice. If needed later, it would lower as: evaluate callee call normally, then pass keyword record as an additional argument. But this is out of scope for 010-2.

### Invoke

`ExprKind::Invoke { callee }`:

Bare invocation (no arguments): `f!`. Used for parameterless effect invocation. Emit `ApplyClosure { closure: callee_operand, args: [] }` or similar, depending on runtime semantics.

### PartialApp

`ExprKind::PartialApp { callee, args: Vec<PartialArg> }` where `PartialArg` is `Expr(ExprId)` or `Placeholder { span }`.

VIR preserves partial application as-is (no desugaring in the CST-to-VIR walk). `UnsupportedShape` in 010-2 — partial application is not supported by the current exec_slice either (it's outside the executable slice subset). If support is needed, the lowering would synthesize a closure: capture the provided `Expr` arguments, take `Placeholder` positions as parameters, and forward all to the callee. But that's future work.

### With expression

`UnsupportedShape` in 010-2. With-expressions (`receiver with { field: val }`) require projecting all fields from the receiver, replacing the updated ones, and reconstructing. There's no dedicated KernelOp for this. Support can be added later if needed by emitting ProjectNamedField for each original field + MakeStruct with overrides.

### Map expression

`UnsupportedShape` in 010-2. There's no `MakeMap` KernelOp. Maps are a build/config layer concept, not part of the executable slice.

### TypeOp

`ExprKind::TypeOp { kind, expr, ty }`:

- `TypeOpKind::Ascription` — type annotation only, no runtime effect. Lower as the inner expression (passthrough).
- `TypeOpKind::As` — `UnsupportedShape`. No runtime conversion ops exist.
- `TypeOpKind::Is` — `UnsupportedShape`. No runtime type-test ops exist.

### Range

`UnsupportedShape` in 010-2. No `MakeRange` KernelOp exists.

### Fallible

`UnsupportedShape` in 010-2. The runtime has no try/catch mechanism for fallible expressions.

### Import

`UnsupportedShape` in 010-2. Module imports are resolved during the build phase, not the executable slice.

### PathLiteral

`ExprKind::PathLiteral { segments }` → emit as `Operand::String(joined_path)` or `TreeFilePath`. Only relevant as arguments to runtime boundary calls (mount paths). Lower as string literal for now.

### Builtin method calls

Calls like `s.len()`, `s.slice(start, end)`, `arr.collect()`, `x.blob()`, `x.which()`:

These are field-access-then-call patterns. In the current exec_slice, they're detected by checking the method name after field resolution fails. In VIR, `ExprKind::Call { callee: ExprKind::Field { receiver, field: Name("len") } }` → emit appropriate op:

- `.len()` → `StringLen`
- `.slice(start, end)` → `StringSlice`
- `.collect()` → `CallMember { method: Collect }`
- `.blob()` → `CallMember { method: Blob }` or `MakeBlob`
- `.which()` → `CallMember { method: Which }`

### Remaining Match patterns

If 010-1 handles simple match (bool/int/binding/wildcard/tuple), 010-2 handles:

- Variant patterns (`Some(x)`, `None`)
- Array patterns (`[first, ...rest]`)
- Or patterns (`a | b`)
- Guard clauses (if any)

These require more complex lowering with nested control flow.

## Runtime node management

The lowering needs a `RuntimeNodeAllocator`:

```rust
struct RuntimeNodeAllocator {
    next_id: u32,
    nodes: BTreeMap<RuntimeNodeId, ExecutableRuntimeNode>,
}
```

When a runtime boundary is detected during lowering, allocate a RuntimeNodeId, construct the ExecutableRuntimeNode with its payload (Exec/Discover/SharedKeyed), and record it. At the end, these nodes are included in the `ExecutableProgram`.

## Files to modify

| File | Change |
|------|--------|
| `crates/vx-vir/src/lower.rs` | Extend: runtime boundaries, remaining expressions (~600 lines) |
| `crates/vx-vir/tests/lower.rs` | Add eval tests |

## Testing

Runtime boundary tests require host integration (file system, process execution), which is complex. Focus on:

1. **Invoke**: `fn f() -> U64 ( 42 ); fn main() -> U64 ( f! )` → 42
2. **TypeOp ascription passthrough**: `fn main() -> U64 ( let x: U64 = 42; x )` → 42 (ascription is a no-op, value passes through)
3. **PathLiteral as string**: verify PathLiteral lowers to a string operand (used in runtime boundary args)

For runtime boundaries specifically, integration testing against the full vixen runner pipeline is more appropriate than unit testing.

## Integration with full pipeline

After 010-2, the VIR lowering path should be able to handle all programs that the current exec_slice handles. The final integration step:

1. Add a `lower_vir_program_for_entry_body` that mirrors `lower_executable_program_for_entry_body`.
2. Wire this into `vx-rt::eval_source` as an alternative path (feature-flagged or configurable).
3. Run the existing `vx-rt` demand_driven tests through both paths and verify identical results.
4. This proves the VIR lowering is a drop-in replacement for the bridge-based lowering.

## Verification

1. `cargo check -p vx-vir`
2. `cargo nextest run -p vx-vir --test lower`
3. `cargo nextest run` — only pre-existing failures
4. Compare `ExecutableProgram` output between old and new paths for all test sources
