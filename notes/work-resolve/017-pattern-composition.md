# 017: Pattern composition — or-patterns and guards

Status: Not started.

## Goal

Finish the higher-level pattern language once the base pattern forms work.

## Depends on

015 and 016.

## Scope

### Or-patterns

`PatternKind::Or(OrPattern { alternatives: Vec<PatternId> })`:

```
match x (
  1 | 2 | 3 => "small",
  _ => "big",
)
```

Lowering: expand to a chain of conditions joined by logical OR.

For `pat_a | pat_b => body`:
1. Lower `pat_a` as a condition (literal test, tag check, etc.) → `cond_a`
2. Lower `pat_b` as a condition → `cond_b`
3. Combine: emit an OR (e.g. `Select { cond: cond_a, then: true, else: cond_b }`) → `combined`
4. Use `combined` as the arm condition in the Select chain

**Binding constraint**: all alternatives in an or-pattern must bind the same names. The lowering must ensure that whichever alternative matches, the bindings are populated. This may require emitting the projections for each alternative and selecting between them.

### Guard clauses

If Vixen supports `match x ( pat if guard_expr => body )`:

1. Lower the pattern as a condition → `pat_cond`
2. If pattern matches, evaluate `guard_expr` → `guard_cond`
3. Combine: `Select { cond: pat_cond, then: guard_cond, else: false }` → `final_cond`
4. Use `final_cond` for the arm

**Side effect concern**: guard expressions should only be evaluated when the pattern matches. The Select chain naturally handles this since the guard is in the "then" branch.

### Variant or-patterns

```
match opt (
  Some { val: 1 } | Some { val: 2 } => "small",
  Some { val } => "other",
  None {} => "none",
)
```

This combines tag checks, field projections, and literal tests. The lowering builds on 015's product-pattern and 016's variant-pattern machinery.

## Why separate

This is where pattern lowering gets subtle. It should sit on top of already-working tuple/struct/variant matching.

## Files to modify

| File | Change |
|------|--------|
| `crates/vx-vir/src/lower.rs` | Extend match lowering for `PatternKind::Or`, guard clauses |
| `crates/vx-vir/tests/lower.rs` | Eval tests |

## Testing

1. Scalar or-patterns: `match n ( 1 | 2 => "yes", _ => "no" )`
2. Variant or-patterns: `match opt ( Some { val: 1 } | Some { val: 2 } => ... )`
3. Guards on successful match only: `match n ( x if x > 10 => ... )`
4. Binding visibility in guarded arms: `match p ( (x, y) if x == y => x )`
5. Or-pattern with shared bindings: `match p ( (1, y) | (2, y) => y )`
6. Guard not evaluated when pattern doesn't match

## Verification

1. `cargo check -p vx-vir`
2. `cargo nextest run -p vx-vir --test lower`
3. `cargo nextest run` — only pre-existing failures
