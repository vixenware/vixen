# 002: Scope Building on VIR

Status: Not started.

## Goal

Migrate scope building from Rowan AST to VIR. Two-pass algorithm stays
(first register all top-level defs, then build inner scopes) but operates
on VIR bodies/bindings instead of syntax nodes.

## Current Implementation

scope.rs (1047 lines) walks Rowan AST, uses TextRange for scope boundaries
and sequential visibility. Forward references handled by two-pass item
registration.

## Work

- Build scope tree from VIR Body/Binding/Expr nodes
- Scope boundaries defined by BodyId (not TextRange)
- Sequential visibility in blocks uses expression ordering (not byte offset)
- Import graph integration stays at SourceId level

## Depends On

000, 001.
