//! Tests for the VIR name resolution pass.

use vx_ir::IrDiagnostic;
use vx_ir::build::VirBuilder;
use vx_ir::expr::ExprKind;
use vx_ir::item::ItemKind;
use vx_ir::pattern::PatternKind;
use vx_ir::resolve::resolve_names;
use vx_ir::scope::DefTarget;
use vx_ir::stmt::StmtKind;
use vx_parser::{SourceId, parse_source_file};

/// Bundled diagnostic — the structural diagnostic together with the source
/// text its primary span points at. Most tests assert on the primary name
/// the diagnostic refers to, so we extract that once here.
#[derive(Debug)]
struct ResolveDiag {
    pub diag: IrDiagnostic,
    pub name: String,
}

fn build_and_resolve(
    source: &str,
) -> (
    vx_ir::arena::VirArena,
    vx_ir::SourceUnitId,
    Vec<ResolveDiag>,
) {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let su_id = program.source_units[0];
    let errors = resolve_names(&mut program.arena);
    let errors = errors
        .into_iter()
        .map(|diag| {
            let span = diag.primary_span();
            let start = u32::from(span.range.start()) as usize;
            let end = u32::from(span.range.end()) as usize;
            let name = source.get(start..end).unwrap_or("").to_string();
            ResolveDiag { diag, name }
        })
        .collect();
    (program.arena, su_id, errors)
}

/// Helper: get the tail expression of a function's root block.
fn fn_tail_expr(
    arena: &vx_ir::arena::VirArena,
    su_id: vx_ir::SourceUnitId,
    name: &str,
) -> vx_ir::ExprId {
    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind
            && f.name == name
        {
            let root = &arena[arena[f.body].root_expr];
            if let ExprKind::Block(b) = &root.kind {
                return b.tail.expect("function should have tail expression");
            }
            panic!("function root is not a block");
        }
    }
    panic!("function '{}' not found", name);
}

/// Helper: get a top-level expression. In the synthetic block, everything is a
/// statement (tail is always None). This finds the expression inside the last
/// ExprStmt, or a specific statement by index.
fn thunk_stmt_expr(
    arena: &vx_ir::arena::VirArena,
    su_id: vx_ir::SourceUnitId,
    stmt_index: usize,
) -> vx_ir::ExprId {
    let body = &arena[arena[su_id].top_level_body];
    let root = &arena[body.root_expr];
    if let ExprKind::Block(b) = &root.kind {
        let stmt_id = b.stmts[stmt_index];
        match &arena[stmt_id].kind {
            StmtKind::Expr(expr_id) => *expr_id,
            StmtKind::Let(l) => l.init,
            other => panic!("expected Expr or Let stmt, got {:?}", other),
        }
    } else {
        panic!("expected Block");
    }
}

/// Helper: count thunk block statements.
fn thunk_stmt_count(arena: &vx_ir::arena::VirArena, su_id: vx_ir::SourceUnitId) -> usize {
    let body = &arena[arena[su_id].top_level_body];
    let root = &arena[body.root_expr];
    if let ExprKind::Block(b) = &root.kind {
        b.stmts.len()
    } else {
        0
    }
}

/// Helper: get the N-th statement in a function's root block.
fn fn_stmt(
    arena: &vx_ir::arena::VirArena,
    su_id: vx_ir::SourceUnitId,
    name: &str,
    index: usize,
) -> vx_ir::StmtId {
    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind
            && f.name == name
        {
            let root = &arena[arena[f.body].root_expr];
            if let ExprKind::Block(b) = &root.kind {
                return b.stmts[index];
            }
        }
    }
    panic!("function '{}' not found", name);
}

// =========================================================================
// 1. Simple binding ref
// =========================================================================

#[test]
fn simple_binding_ref() {
    let (arena, su_id, errors) = build_and_resolve("fn main() ( let x = 1; x )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    let tail_id = fn_tail_expr(&arena, su_id, "main");
    let tail = &arena[tail_id];
    if let ExprKind::NameRef(nr) = &tail.kind {
        assert!(
            matches!(nr.resolved, Some(DefTarget::Binding(_))),
            "x should resolve to Binding: {:?}",
            nr.resolved
        );
    } else {
        panic!("expected NameRef tail, got {:?}", tail.kind);
    }
}

// =========================================================================
// 2. Function ref from thunk block
// =========================================================================

#[test]
fn function_ref_from_thunk_block() {
    let (arena, su_id, errors) = build_and_resolve("fn f() ( 1 )\nf");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    // Last thunk stmt is the expression `f`.
    let count = thunk_stmt_count(&arena, su_id);
    let expr_id = thunk_stmt_expr(&arena, su_id, count - 1);
    if let ExprKind::NameRef(nr) = &arena[expr_id].kind {
        assert!(
            matches!(nr.resolved, Some(DefTarget::Item(_))),
            "f should resolve to Item: {:?}",
            nr.resolved
        );
    } else {
        panic!("expected NameRef");
    }
}

// =========================================================================
// 3. Unresolved name
// =========================================================================

#[test]
fn unresolved_name() {
    let (_, _, errors) = build_and_resolve("fn main() ( x )");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].name, "x");
    assert!(matches!(errors[0].diag, IrDiagnostic::Unresolved { .. }));
}

// =========================================================================
// 4. Let not visible in own init
// =========================================================================

#[test]
fn let_not_visible_in_own_init() {
    let (arena, su_id, errors) = build_and_resolve("fn main() ( let x = 1; let x = x + 1 )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    // The second let's init contains x + 1. The x should resolve to the
    // first binding, not the second (which isn't yet visible).
    let stmt_id = fn_stmt(&arena, su_id, "main", 1); // second let
    if let StmtKind::Let(l) = &arena[stmt_id].kind {
        // The init should be a Binary with lhs = NameRef(x)
        let init = &arena[l.init];
        if let ExprKind::Binary(bin) = &init.kind {
            let lhs = &arena[bin.lhs];
            if let ExprKind::NameRef(nr) = &lhs.kind {
                match nr.resolved {
                    Some(DefTarget::Binding(bid)) => {
                        assert_eq!(arena[bid].name, "x");
                    }
                    other => panic!("expected Binding, got {:?}", other),
                }
            } else {
                panic!("expected NameRef lhs");
            }
        }
    }
}

// =========================================================================
// 5. Nested function cutoff
// =========================================================================

#[test]
fn nested_function_cutoff() {
    let (_, _, errors) = build_and_resolve("fn main() ( let a = 1; fn f() ( a + b ); let b = 2 )");
    // 'b' should be unresolved inside f (cutoff prevents seeing it).
    // 'a' should resolve fine.
    assert_eq!(
        errors.len(),
        1,
        "expected 1 error: {:?}",
        errors.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert_eq!(errors[0].name, "b");
    assert!(matches!(errors[0].diag, IrDiagnostic::Unresolved { .. }));
}

// =========================================================================
// 6. Block-local self-recursion
// =========================================================================

#[test]
fn block_local_self_recursion() {
    let (arena, su_id, errors) = build_and_resolve("fn main() ( fn f() ( f ) )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    // Find f's body, check that f in its tail resolves to Item.
    let main_stmt = fn_stmt(&arena, su_id, "main", 0);
    if let StmtKind::Item(item_id) = &arena[main_stmt].kind
        && let ItemKind::Function(f) = &arena[*item_id].kind
    {
        let root = &arena[arena[f.body].root_expr];
        if let ExprKind::Block(b) = &root.kind {
            let tail_id = b.tail.expect("f should have tail");
            if let ExprKind::NameRef(nr) = &arena[tail_id].kind {
                assert!(
                    matches!(nr.resolved, Some(DefTarget::Item(_))),
                    "f should resolve to Item (self-recursion): {:?}",
                    nr.resolved
                );
            }
        }
    }
}

// =========================================================================
// 7. Block-local shadowing
// =========================================================================

#[test]
fn block_local_shadowing() {
    let (arena, su_id, errors) = build_and_resolve("fn main() ( let x = 1; let x = 2; x )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    let tail_id = fn_tail_expr(&arena, su_id, "main");
    if let ExprKind::NameRef(nr) = &arena[tail_id].kind {
        match nr.resolved {
            Some(DefTarget::Binding(bid)) => {
                // Should be the second binding. The second binding has a
                // higher index than the first.
                assert_eq!(arena[bid].name, "x");
            }
            other => panic!("expected Binding, got {:?}", other),
        }
    }
}

// =========================================================================
// 8-9. Top-level shadowing (both orders)
// =========================================================================

#[test]
fn top_level_binding_shadows_item_binding_first() {
    let (arena, su_id, errors) = build_and_resolve("let f = 1\nfn f() ( 2 )\nf");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    let count = thunk_stmt_count(&arena, su_id);
    let expr_id = thunk_stmt_expr(&arena, su_id, count - 1);
    if let ExprKind::NameRef(nr) = &arena[expr_id].kind {
        assert!(
            matches!(nr.resolved, Some(DefTarget::Binding(_))),
            "binding should shadow item: {:?}",
            nr.resolved
        );
    }
}

#[test]
fn top_level_binding_shadows_item_item_first() {
    let (arena, su_id, errors) = build_and_resolve("fn f() ( 1 )\nlet f = 2\nf");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    let count = thunk_stmt_count(&arena, su_id);
    let expr_id = thunk_stmt_expr(&arena, su_id, count - 1);
    if let ExprKind::NameRef(nr) = &arena[expr_id].kind {
        assert!(
            matches!(nr.resolved, Some(DefTarget::Binding(_))),
            "binding should shadow item regardless of order: {:?}",
            nr.resolved
        );
    }
}

// =========================================================================
// 10. Valid assign
// =========================================================================

#[test]
fn valid_assign_resolves() {
    let (arena, su_id, errors) = build_and_resolve("fn main() ( let x = 1; x = 2 )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    let stmt_id = fn_stmt(&arena, su_id, "main", 1); // assign stmt
    if let StmtKind::Assign(a) = &arena[stmt_id].kind {
        assert!(a.resolved.is_some(), "assign should be resolved");
        assert_eq!(arena[a.resolved.unwrap()].name, "x");
    } else {
        panic!("expected Assign stmt, got {:?}", arena[stmt_id].kind);
    }
}

// =========================================================================
// 11. Invalid assign target
// =========================================================================

#[test]
fn invalid_assign_target() {
    let (_, _, errors) = build_and_resolve("fn main() ( fn x() ( 1 ); x = 2 )");
    assert_eq!(
        errors.len(),
        1,
        "expected 1 error: {:?}",
        errors.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert_eq!(errors[0].name, "x");
    assert!(
        matches!(errors[0].diag, IrDiagnostic::InvalidTarget { .. }),
        "expected InvalidTarget: {:?}",
        errors[0].diag
    );
}

// =========================================================================
// 12. Namespace resolution
// =========================================================================

#[test]
fn namespace_sibling_resolution() {
    // Functions inside a namespace should see each other (namespace is order-independent).
    let (_, _, errors) = build_and_resolve("namespace Foo {\nfn a() ( 1 )\nfn b() ( a )\n}");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);
}

// =========================================================================
// 13-14. Special names
// =========================================================================

#[test]
fn dollar_ident_skipped() {
    let (_, _, errors) = build_and_resolve("fn main() ( $0 )");
    assert!(
        errors.is_empty(),
        "dollar idents should not produce errors: {:?}",
        errors
    );
}

#[test]
fn underscore_skipped() {
    let (_, _, errors) = build_and_resolve("fn main() ( _ )");
    assert!(
        errors.is_empty(),
        "underscore should not produce errors: {:?}",
        errors
    );
}

// =========================================================================
// 15. Variant pattern resolves
// =========================================================================

#[test]
fn variant_pattern_resolves() {
    let (arena, su_id, errors) = build_and_resolve(
        "enum Option { struct Some { value: U64 }, struct None }\nfn main() ( match x { None => 1 } )",
    );
    // 'x' will be unresolved — that's expected. Filter for non-x errors.
    let non_x_errors: Vec<_> = errors.iter().filter(|e| e.name != "x").collect();
    assert!(
        non_x_errors.is_empty(),
        "expected no errors besides 'x': {:?}",
        non_x_errors.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // Find the match arm pattern and check it resolved.
    // Walk main's root block tail (the match expr).
    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind
            && f.name == "main"
        {
            let root = &arena[arena[f.body].root_expr];
            if let ExprKind::Block(b) = &root.kind {
                let tail = b.tail.expect("should have tail");
                if let ExprKind::Match(m) = &arena[tail].kind {
                    let pat = &arena[m.arms[0].pattern];
                    if let PatternKind::Variant(vp) = &pat.kind {
                        assert!(vp.resolved.is_some(), "variant pattern should be resolved");
                    }
                }
            }
        }
    }
}

// =========================================================================
// 16. Struct literal resolves
// =========================================================================

#[test]
fn struct_literal_resolves() {
    let (arena, su_id, errors) =
        build_and_resolve("struct Point { x: F64, y: F64 }\nfn main() ( Point { x: 1, y: 2 } )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    let tail_id = fn_tail_expr(&arena, su_id, "main");
    if let ExprKind::StructLiteral(sl) = &arena[tail_id].kind {
        assert!(sl.resolved.is_some(), "struct literal should be resolved");
        let item_id = sl.resolved.unwrap();
        assert!(
            matches!(arena[item_id].kind, ItemKind::Struct(_)),
            "resolved item should be Struct"
        );
    } else {
        panic!("expected StructLiteral");
    }
}

// =========================================================================
// 17. Struct literal resolving to non-struct → InvalidTarget
// =========================================================================

#[test]
fn struct_literal_non_struct_invalid() {
    let (_, _, errors) = build_and_resolve("fn main() ( fn Point() ( 1 ); Point { x: 1 } )");
    let invalid: Vec<_> = errors
        .iter()
        .filter(|e| e.name == "Point" && matches!(e.diag, IrDiagnostic::InvalidTarget { .. }))
        .collect();
    assert_eq!(
        invalid.len(),
        1,
        "expected InvalidTarget for Point: {:?}",
        errors.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

// =========================================================================
// 18. Variant pattern resolving to non-enum item → InvalidTarget
// =========================================================================

#[test]
fn variant_pattern_non_enum_invalid() {
    // Point is a struct, not an enum — using it as a variant pattern is invalid.
    let (_, _, errors) =
        build_and_resolve("struct Point { x: F64, y: F64 }\nfn main() ( match x { Point => 1 } )");
    // 'x' will be unresolved. 'Point' in the pattern should be InvalidTarget
    // (it resolves to a struct item, not an enum/variant).
    let invalid: Vec<_> = errors
        .iter()
        .filter(|e| e.name == "Point" && matches!(e.diag, IrDiagnostic::InvalidTarget { .. }))
        .collect();
    assert_eq!(
        invalid.len(),
        1,
        "expected InvalidTarget for Point variant pattern: {:?}",
        errors.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

// =========================================================================
// 19. Top-level sibling functions
// =========================================================================

#[test]
fn top_level_sibling_functions() {
    let (arena, su_id, errors) = build_and_resolve("fn a() ( 1 )\nfn b() ( a )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    // Check that 'a' inside b resolves to Item.
    let tail_id = fn_tail_expr(&arena, su_id, "b");
    if let ExprKind::NameRef(nr) = &arena[tail_id].kind {
        assert!(
            matches!(nr.resolved, Some(DefTarget::Item(_))),
            "a should resolve to Item from b: {:?}",
            nr.resolved
        );
    }
}

// =========================================================================
// 20. RHS of assignment is resolved
// =========================================================================

#[test]
fn assign_rhs_resolved() {
    let (arena, su_id, errors) = build_and_resolve("fn main() ( let x = 1; let y = 2; x = y )");
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);

    // The assign stmt's value (y) should be resolved.
    let stmt_id = fn_stmt(&arena, su_id, "main", 2); // assign stmt
    if let StmtKind::Assign(a) = &arena[stmt_id].kind {
        assert!(a.resolved.is_some(), "assign target should resolve");
        // Check the RHS expression (y) is also resolved.
        if let ExprKind::NameRef(nr) = &arena[a.value].kind {
            assert!(
                matches!(nr.resolved, Some(DefTarget::Binding(_))),
                "y in RHS should resolve: {:?}",
                nr.resolved
            );
        }
    }
}
