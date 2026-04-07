# 008: Demand Analysis on VIR

Status: Not started.

## Goal

Reimplement demand propagation on VIR. This is several sub-items:

- Product/projection fact extraction from VIR expressions
- Demand roots (entry points)
- Local demand propagation (within a function)
- Cross-function demand propagation (through call sites)
- Reachability analysis (which functions are reachable from entry)

## Current Architecture (to replicate on VIR)

Core model: `Observation` enum — `NoObs | NeedValue | NeedCallable | NeedProduct(BTreeMap<FieldKey, Observation>)`.
Tree-structured demand tracking per field.

Three seeding phases (bridge_to_analysis.rs):
1. Runtime boundary marking (lines 25-74) — Exec/Discover/SharedKeyed calls → extracted runtime nodes
2. Local lazy field seeding (lines 390-630) — projection chains within a function → LazyProductField for unused siblings
3. Cross-function lazy field seeding (lines 678-830) — callee projects subset of param fields → lazy siblings at call site

Propagation: backward from entry points (main/build), queue through source_local_call_edges,
marking callees with NeedValue. Results consumed in lowering for entry selection and lazy→thunk wrapping.

## Testing Strategy

Text-based snapshot format for analysis results. Each test is a .vx source
file paired with an .expected snapshot. The dump format shows per-function:
demand observations, lazy fields, call edges, reachability.

This lets us swap the implementation (old bridge-based vs new VIR-based)
and verify both produce the same analysis results.

## Depends On

005 (type checking), 007 (capture analysis).
