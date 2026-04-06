# 009: Toward cc-version.vx through the real pipeline

Close the gap to `zoo/cc-version.vx` — the near-term trajectory target from
eval3-axes.md.

## Good news

cc-version.vx does NOT use enums or match. It's mostly build-shaped:
discover, exec, observer closures, struct field access, if/else, Tree literal.

The existing exec_slice lowering already handles most of this. ProgramEvaluator
already passes tests for discover+exec+string ops.

## What cc-version needs

- Struct construction with named fields ✓ (exec_slice handles this)
- Struct field access ✓
- `discover` with closure ✓ (AwaitRuntime)
- `exec` with observer closure ✓ (AwaitRuntime)
- `if/else` in observer ✓ (Select KernelOp)
- `Tree %{...}` construction ✓ (BuildTree KernelOp)
- Method chaining `.collect().blob()` ✓ (CallMember KernelOp)
- `fail` with string interpolation — needs checking
- `where` clause on exec — needs checking
- `import()` for packages — needs checking
- `executor_class()` — needs checking

## The real gap

The gap isn't language features — it's making cc-version run through a
**demand-driven** runtime instead of ProgramEvaluator's eager execution.
Once 008 is done, cc-version should be close.

## Depends on

007 (demand-driven runtime), possibly 006 (thunks for lazy observer fields)

## Status

Not started.
