# 005: Type Checking on VIR

Status: Not started.

## Goal

Migrate type checker from Rowan AST to VIR. Stores HashMap<ExprId, Ty>
instead of HashMap<SourceSpan, Ty>.

## Current Implementation

type_check.rs (2278 lines). Heavily coupled to Rowan AST types.

## Decomposition

Split into modules by expression category:
- literals.rs (~50 lines)
- names.rs (~100 lines) — name refs, binding types
- calls.rs (~500 lines) — signature matching, overloads, generics
- fields.rs (~300 lines) — field projection, method resolution
- closures.rs (~200 lines) — param binding, body inference
- control_flow.rs (~200 lines) — if, match, blocks
- operators.rs (~200 lines) — binary, prefix, pipe
- collections.rs (~300 lines) — array, tuple, record, map, struct literal
- special.rs (~100 lines) — with, fallible, interpolated string
- mod.rs — thin dispatcher + infrastructure (substitution, unification, errors)

## Key Changes

- All results keyed by ExprId, not SourceSpan
- Operator kinds pre-computed in VIR (BinOp enum, not SyntaxKind scanning)
- Literal kinds pre-computed in VIR (LiteralKind enum, not token kind checks)
- Special forms (trap, discover) identified in VIR as BuiltinCall
- No .syntax(), .text_range(), .text() calls

## Depends On

003 (name resolution), 004 (type collection).
