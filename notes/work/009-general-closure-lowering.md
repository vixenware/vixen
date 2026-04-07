# 009: General closure lowering

The exec_slice closure lowering only supports `(x) => x + captured` — a
single-param closure whose body is a binary add with one captured binding.

## What changed

- `synthesize_closure_body`: proper closure body synthesis mirroring
  `synthesize_thunk_body`, with capture pruning (only captures actually
  referenced in the body are included) and slot remapping.
- `AllocClosure` KernelOp: `{ body, captures, param_count }` — at runtime,
  captures + args become the body's params.
- `ApplyClosure` KernelOp: `{ closure, args }` — forces the closure value,
  prepends captures to args, calls `eval_body`.
- NameRef → top-level function: when a NameRef resolves to a top-level
  function used as a value (not called), emit `AllocClosure` with 0 captures.
- `reachable_body_slice` enhanced to include functions referenced as values
  (not just called), so they get lowered.
- Callee fallthrough for calls with parameter callees (e.g. `f(x)` where
  `f` is a parameter) — resolves callee by span when no BridgeExpr exists.

## Fixtures unblocked

- 10_closure_call
- 11_closure_capture (zero-arg closure)
- 16_higher_order (fn passed as arg)
- 18_compose (`(x) => g(f(x))`)
- 21_closure_returns_closure (`(x) => (y) => x + y`)
- 24_closure_in_tuple

## Scorecard

36/38 slice0 fixtures pass. Remaining 2 are inline product/struct laziness
(work item 010).

## Status

Done.
