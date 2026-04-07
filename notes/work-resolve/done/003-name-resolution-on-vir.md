# 003: Name Resolution on VIR

Status: Not started.

## Goal

Migrate name resolution from Rowan AST to VIR. Walk VIR expressions,
resolve NameRef nodes to BindingId/DefId.

## Current Implementation

resolve.rs (169 lines) walks CST finding NameRefExpr nodes, calls
scopes.lookup_at() for each. Simple and mechanical.

## Work

- Walk VIR expressions for ExprKind::NameRef
- Resolve each to its target binding or definition
- Store results on the VIR node (NameRef.resolved) or in a side map
- No span-based lookups

## Depends On

002 (scope building).
