# 014: Lower runtime-boundary where payloads

Status: Not started.

## Goal

Finish the one big hole left in 010-2: `exec`/`discover`/`shared_keyed` when wrapped in `where { ... }`.

## Depends on

010-2.

## Scope

- Lower `mounts` keyword → `Vec<(TreeFilePath, RuntimeBoundaryInputSpec)>` in `AwaitRuntime`
- Lower `writable_roots` keyword → `Vec<TreeFilePath>` in `AwaitRuntime`
- Lower `coarse_mounts` keyword → `Vec<RuntimeBoundaryInputSpec>` in `AwaitRuntime`
- Lower observer closures (e.g. `observer: (proc, _vfs) => ...`) → synthesize closure body, pass as `RuntimeBoundaryInputSpec::Closure`
- Set `has_where_clause`, `has_mounts`, `has_writable`, `has_coarse_mounts` flags in `ExecBoundaryPayload`
- Keep non-runtime `Where` (keyword argument passing) as `UnsupportedShape`

### Lowering strategy

When `ExprKind::Where { callee, keywords }` is encountered:

1. Check if `callee` is a `Call` whose callee resolves to a runtime boundary function.
2. If yes, lower the inner call to `AwaitRuntime` as in 010-2, but additionally:
   - Walk the `keywords` expression (expected to be a record literal) for known keys: `mounts`, `writable_roots`, `coarse_mounts`, `observer`, `executor`.
   - For `mounts`: expect a map/record where keys are path literals and values are expressions. Lower each to `(TreeFilePath, RuntimeBoundaryInputSpec)`.
   - For `writable_roots`: expect an array of path literals. Lower each to `TreeFilePath`.
   - For `observer`: expect a closure expression. Lower to `RuntimeBoundaryInputSpec::Closure` with source and `ExecutableClosure`.
3. If callee is NOT a runtime boundary, emit `UnsupportedShape`.

### ExecutableClosure

`RuntimeBoundaryInputSpec::Closure` requires an `ExecutableClosure`:
```rust
pub struct ExecutableClosure {
    pub body: BodyId,
    pub captures: Vec<Operand>,
    pub param_count: usize,
}
```
This is essentially the same data as `AllocClosure` but packaged for the runtime boundary protocol.

## Why separate

This is high-value and self-contained. It does not require changing the runtime value model. It completes the runtime boundary story that 010-2 started.

## Files to modify

| File | Change |
|------|--------|
| `crates/vx-vir/src/lower.rs` | Implement `lower_where`, extend `lower_runtime_boundary_call` |
| `crates/vx-vir/tests/lower.rs` | Integration tests (limited — runtime boundaries need host) |

## Testing

- `exec(...) where { mounts: ... }` — verify `AwaitRuntime` has non-empty mounts
- `exec(...) where { writable_roots: ... }` — verify writable_roots populated
- Observer closure attached and invoked
- Invalid where keys rejected cleanly
- Non-runtime `Where` produces `UnsupportedShape`

Note: Full integration testing requires the vixen runner pipeline. Unit tests can verify the `ExecutableProgram` structure without executing `AwaitRuntime`.

## Verification

1. `cargo check -p vx-vir`
2. `cargo nextest run -p vx-vir --test lower`
3. `cargo nextest run` — only pre-existing failures
