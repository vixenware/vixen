# 010: Lowering from VIR to KernelOp

Status: Not started.

## Goal

Lower VIR to KernelOp bytecode. Pattern match on ExprKind, emit ops.

## Key Improvements Over Current exec_slice.rs

- Global thunk/closure body ID allocation (not per-function-starting-at-10000)
- Captures are explicit from capture analysis (007) — no "capture everything and prune"
- Complete slot remapping for ALL KernelOp variants
- No span resurrection, no name-based fallbacks
- Builtins modeled as ExprKind::BuiltinCall, not string-compared token text
- Proper scoping from VIR body tree (not flat lookup tables)

## KernelOp Architecture (from research)

KernelOp: ~30-variant enum (exec_slice.rs:122-306). SSA-like, each op has `dst: SlotId`.
Categories: constants, closures/thunks, arithmetic, projections, calls, runtime boundaries.

Slots: u32-based, per-body namespace starting from 0. Params get 0..N, temps on demand.
No global slot counter — each body independent. Multi-file: all files share one BodyId
namespace, but slots are per-body.

Lowering flow: `lower_executable_program` → per-body `BodyLowering` → recursive `lower_expr()` →
KernelOp sequence + synthesized thunk/closure bodies.

Thunks/closures: `synthesize_thunk_body` creates child BodyLowering with captured bindings
remapped to fresh param slots, prunes unused captures.

## Resolved Questions

- Slots are per-body, not global. Multi-file just means more BodyIds.
- KernelOp is SSA-like bytecode interpreted by vx-rt. Each variant produces one value to a SlotId.

## Depends On

008, 009 (analysis + lazy seeding).
