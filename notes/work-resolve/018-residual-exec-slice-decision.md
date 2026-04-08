# 018: Residual executable-slice decision item

Status: Not started.

## Goal

Decide which currently unsupported constructs should actually be lowered, and which should remain outside the executable slice permanently.

## Depends on

None strictly, but better after 014–017.

## Context

After 010-0 through 010-2 and 014–017, the VIR lowering covers: literals, bindings, arithmetic, control flow, calls, closures, products, lazy thunks, field projection, match (all pattern forms), runtime boundaries with where payloads, builtin methods, path literals, type ascription, and invoke.

The following expression kinds are still `UnsupportedShape`:

| Expression | Current status | Notes |
|-----------|---------------|-------|
| `PartialApp` | UnsupportedShape | `f(a, _)` — partial application. Could desugar to closure. |
| `With` | UnsupportedShape | `x with { field: val }` — functional record update. No dedicated op. |
| `Map` | UnsupportedShape | `%{ key: val }` — map literal. No `MakeMap` op exists. |
| `Range` | UnsupportedShape | `1..10` — range literal. No `MakeRange` op exists. |
| `Fallible` | UnsupportedShape | `try expr` — error handling. No try/catch mechanism. |
| `Import` | UnsupportedShape | `import "path"` — resolved during build, not executable. |
| `TypeOp::As` | UnsupportedShape | `expr as T` — type cast. No conversion ops. |
| `TypeOp::Is` | UnsupportedShape | `expr is T` — type test. No type-test ops. |
| `Index` | UnsupportedShape | `arr[i]` — array indexing. |

## Scope

For each construct, choose exactly one disposition:

### A: Lower it
The construct has clear runtime semantics and enough users to justify the work. Write a follow-up work item.

### B: Desugar it earlier
The construct can be expressed in terms of things we already lower. Add a desugaring pass before lowering (or in VIR build).

Candidates:
- **PartialApp**: desugar `f(a, _)` → `(x) => f(a, x)` (closure synthesis)
- **With**: desugar `x with { field: val }` → project all fields from x, override, reconstruct

### C: Keep UnsupportedShape permanently
The construct is a build/config layer concept, not part of the executable slice. It's an error for it to appear in lowered code.

Candidates:
- **Import**: always resolved before lowering
- **Map**: build-time config, not executable
- **Range**: possibly desugar, possibly keep out

## Deliverable

A decision document (update this file) recording the disposition of each construct, with rationale. For any construct assigned disposition A or B, create the corresponding work item.

## Testing

N/A — this is a decision item, not an implementation item. Tests come from the follow-up work items.
