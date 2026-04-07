# 013: Delete vx-analysis

Status: Not started.

## Goal

Once all consumers are migrated, delete the vx-analysis crate entirely.
The bridge, collect_* functions, body_for_range, expr_id_by_span,
span-based type joins — all gone.

## Depends On

012 (all consumers migrated).
