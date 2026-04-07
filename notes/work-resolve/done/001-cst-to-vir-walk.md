# 001: CST to VIR Walk

Status: Not started.

## Goal

Single recursive visitor that descends the Rowan AST and produces VIR.
Replaces collect_bodies, collect_exprs, body_for_range, expr_id_by_span.

## Behavior

- Tracks current BodyId as it descends into functions/closures
- Allocates ExprId/BodyId/BindingId from arenas
- Records source spans as metadata on every node
- Closures and nested functions become separate Body entries with parent links
- Pre-computes operator kinds, literal kinds (no raw token inspection later)
- Identifies builtins (trap, Blob, Tree, etc.) during walk, stores as ExprKind::BuiltinCall

## Output

A VIR tree: Vec<Body>, arena of Expr, arena of Binding.
Every node has an ID, a source span, and structural parent info.

## Testing

Round-trip tests: parse source, build VIR, assert structure.
Text dump format for snapshot testing.

## Depends On

000 (VIR data structures).
