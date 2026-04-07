# 012: Match/Enum Lowering + Port `.styx` Fixtures

Extend exec_slice to lower match expressions and enum construction, then port
the 16 slice1 .styx fixtures through the real pipeline.

Status: Not started.

## The gap

`Expr::Match` is NOT handled in exec_slice `lower_expr` — it falls through to
"outside restricted executable slice" error. All 16 slice1 fixtures use match.

## What needs to change in exec_slice

- Add match expression lowering: scrutinee evaluation, variant tag check,
  field binding, branch selection
- New KernelOps: possibly `MatchVariant`, `ExtractField`, or lower match
  to existing `Select` + `ProjectField` ops
- Enum variant construction (may already partially work)

## Also needed for some slice0 fixtures

- Multi-arg callable calls (Program D pattern — `apply(f, x)`)
- Multi-param closures (exec_slice line 1686 restricts to 1)
- Complex closure bodies (beyond `x + captured` form)

## Depends on

014 (demand propagation cleanup), 015 (kernel entry exprs/types).
