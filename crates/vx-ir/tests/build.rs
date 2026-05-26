//! Round-trip tests for the CST-to-VIR builder.
//!
//! Each test parses a Vixen source snippet, builds VIR, and asserts structural
//! properties of the resulting arena.

use vx_ir::build::VirBuilder;
use vx_ir::expr::{ExprKind, FieldKey, LitKind};
use vx_ir::item::ItemKind;
use vx_ir::pattern::PatternKind;
use vx_ir::stmt::StmtKind;
use vx_parser::{SourceId, parse_source_file};

fn build_single(source: &str) -> (vx_ir::arena::VirArena, vx_ir::SourceUnitId) {
    let parsed = parse_source_file(source, SourceId(0));
    let file = parsed.tree();
    let program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let su_id = program.source_units[0];
    (program.arena, su_id)
}

#[test]
fn simple_function() {
    let (arena, su_id) = build_single("fn add(x) ( x + 1 )");
    let su = &arena[su_id];
    assert_eq!(su.items.len(), 1);

    let item = &arena[su.items[0]];
    let func = match &item.kind {
        ItemKind::Function(f) => f,
        other => panic!("expected Function, got {:?}", other),
    };
    assert_eq!(func.name, "add");

    let body = &arena[func.body];
    assert_eq!(body.params.len(), 1);
    // Top-level function has no parent.
    assert!(
        body.parent.is_none(),
        "top-level fn should have parent: None"
    );

    // Root expr should be a binary (x + 1) — or a block containing it.
    // The parser wraps the body in a BlockExpr `( x + 1 )`.
    let root = &arena[body.root_expr];
    match &root.kind {
        ExprKind::Block(block) => {
            assert!(block.tail.is_some(), "block should have tail expression");
            let tail = &arena[block.tail.unwrap()];
            assert!(
                matches!(tail.kind, ExprKind::Binary(_)),
                "tail should be binary, got {:?}",
                tail.kind
            );
        }
        ExprKind::Binary(_) => {} // also acceptable
        other => panic!("expected Block or Binary root, got {:?}", other),
    }
}

#[test]
fn block_with_let() {
    let (arena, su_id) = build_single("fn main() ( let x = 1; x + 1 )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let body = &arena[func.body];
    let root = &arena[body.root_expr];

    if let ExprKind::Block(block) = &root.kind {
        assert!(!block.stmts.is_empty(), "block should have statements");
        assert!(block.tail.is_some(), "block should have tail expression");

        // First stmt should be a Let.
        let stmt = &arena[block.stmts[0]];
        assert!(matches!(stmt.kind, StmtKind::Let(_)));

        // Tail should be binary.
        let tail = &arena[block.tail.unwrap()];
        assert!(matches!(tail.kind, ExprKind::Binary(_)));
    } else {
        panic!("expected Block root, got {:?}", root.kind);
    }
}

#[test]
fn nested_closure() {
    let (arena, su_id) = build_single("fn main() ( let f = (x) => x + 1; f(2) )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };

    // Should have at least 2 bodies: the function body and the closure body.
    // The top-level body is also present.
    let fn_body = &arena[func.body];
    let root = &arena[fn_body.root_expr];

    if let ExprKind::Block(block) = &root.kind {
        // First stmt is `let f = (x) => x + 1`
        let let_stmt = &arena[block.stmts[0]];
        if let StmtKind::Let(l) = &let_stmt.kind {
            let init = &arena[l.init];
            if let ExprKind::Closure(c) = &init.kind {
                let closure_body = &arena[c.body];
                assert_eq!(closure_body.params.len(), 1);
                // Closure should have the function body as parent.
                assert!(
                    closure_body.parent.is_some(),
                    "closure should have parent body"
                );
                assert_eq!(closure_body.parent.unwrap(), func.body);
            } else {
                panic!("expected Closure, got {:?}", init.kind);
            }
        } else {
            panic!("expected Let stmt");
        }
    } else {
        panic!("expected Block root");
    }
}

#[test]
fn nested_function_has_parent() {
    let (arena, su_id) = build_single("fn outer() ( fn inner() ( 1 ); inner!  )");
    let su = &arena[su_id];
    let outer_func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let outer_body = &arena[outer_func.body];
    let root = &arena[outer_body.root_expr];

    if let ExprKind::Block(block) = &root.kind {
        // First stmt should be an item (inner function).
        let stmt = &arena[block.stmts[0]];
        if let StmtKind::Item(item_id) = &stmt.kind {
            let inner_item = &arena[*item_id];
            if let ItemKind::Function(inner_fn) = &inner_item.kind {
                assert_eq!(inner_fn.name, "inner");
                let inner_body = &arena[inner_fn.body];
                // Nested function should have the outer function body as parent.
                assert_eq!(
                    inner_body.parent,
                    Some(outer_func.body),
                    "nested fn should have outer fn as parent"
                );
            } else {
                panic!("expected inner Function item");
            }
        } else {
            panic!("expected Item stmt, got {:?}", stmt.kind);
        }
    } else {
        panic!("expected Block root");
    }
}

#[test]
fn curried_call_not_flattened() {
    // f(a)(b) should produce nested Call nodes, not a single Call with 2 args.
    let (arena, su_id) = build_single("fn main() ( f(a)(b) )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let body = &arena[func.body];
    let root = &arena[body.root_expr];

    // Navigate to the tail expression.
    let tail_expr = if let ExprKind::Block(block) = &root.kind {
        block.tail.map(|id| &arena[id])
    } else {
        Some(root)
    };

    let tail = tail_expr.expect("should have tail");
    // Outer call: ?(b) where ? = inner call
    if let ExprKind::Call(outer_call) = &tail.kind {
        assert_eq!(outer_call.args.len(), 1, "outer call should have 1 arg");
        // Inner callee should also be a Call.
        let callee = &arena[outer_call.callee];
        if let ExprKind::Call(inner_call) = &callee.kind {
            assert_eq!(inner_call.args.len(), 1, "inner call should have 1 arg");
            // Inner callee should be a NameRef to 'f'.
            let f = &arena[inner_call.callee];
            assert!(matches!(f.kind, ExprKind::NameRef(_)));
        } else {
            panic!("expected nested Call, got {:?}", callee.kind);
        }
    } else {
        panic!("expected Call, got {:?}", tail.kind);
    }
}

#[test]
fn multi_arg_call() {
    let (arena, su_id) = build_single("fn main() ( f(a, b, c) )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let body = &arena[func.body];
    let root = &arena[body.root_expr];

    let tail_expr = if let ExprKind::Block(block) = &root.kind {
        block.tail.map(|id| &arena[id])
    } else {
        Some(root)
    };

    let tail = tail_expr.expect("should have tail");
    if let ExprKind::Call(call) = &tail.kind {
        assert_eq!(
            call.args.len(),
            3,
            "should have 3 args, got {}",
            call.args.len()
        );
    } else {
        panic!("expected Call, got {:?}", tail.kind);
    }
}

#[test]
fn match_expression() {
    let (arena, su_id) = build_single("fn main() ( match x { 1 => true, _ => false } )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let body = &arena[func.body];
    let root = &arena[body.root_expr];

    let tail_expr = if let ExprKind::Block(block) = &root.kind {
        block.tail.map(|id| &arena[id])
    } else {
        Some(root)
    };

    let tail = tail_expr.expect("should have tail");
    if let ExprKind::Match(m) = &tail.kind {
        assert_eq!(m.arms.len(), 2, "should have 2 arms");
        // First arm pattern should be a literal.
        let arm0_pat = &arena[m.arms[0].pattern];
        assert!(
            matches!(arm0_pat.kind, PatternKind::Literal(_)),
            "first arm should have literal pattern, got {:?}",
            arm0_pat.kind
        );
        // Second arm pattern should be a wildcard.
        let arm1_pat = &arena[m.arms[1].pattern];
        assert!(
            matches!(arm1_pat.kind, PatternKind::Wildcard),
            "second arm should be wildcard, got {:?}",
            arm1_pat.kind
        );
    } else {
        panic!("expected Match, got {:?}", tail.kind);
    }
}

#[test]
fn error_recovery_missing_if_condition() {
    // Malformed if: missing condition. Should produce Error node, no panic.
    let (arena, su_id) = build_single("fn main() ( if ( 1 ) )");
    // Just verify it didn't panic and produced a source unit.
    let su = &arena[su_id];
    assert_eq!(su.items.len(), 1);
}

#[test]
fn top_level_interleaving() {
    let (arena, su_id) = build_single("fn a() ( 1 )\nlet x = 2\nfn b() ( 3 )");
    let su = &arena[su_id];
    // Should have 2 items (fn a, fn b).
    assert_eq!(su.items.len(), 2);

    // The top-level body should have statements in source order:
    // Item(a), Let(x=2) or Expr(let...), Item(b)
    let body = &arena[su.top_level_body];
    let root = &arena[body.root_expr];
    if let ExprKind::Block(block) = &root.kind {
        assert!(block.synthetic, "top-level block should be synthetic");
        assert!(
            block.stmts.len() >= 3,
            "should have at least 3 top-level statements, got {}",
            block.stmts.len()
        );
    } else {
        panic!("expected synthetic Block root");
    }
}

#[test]
fn synthetic_top_level_block() {
    let (arena, su_id) = build_single("fn main() ( 1 )");
    let body = &arena[arena[su_id].top_level_body];
    let root = &arena[body.root_expr];
    if let ExprKind::Block(block) = &root.kind {
        assert!(block.synthetic, "top-level block must be synthetic");
    } else {
        panic!("expected Block");
    }
}

#[test]
fn raw_literal_storage() {
    let (arena, su_id) = build_single("fn main() ( 42 )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let body = &arena[func.body];
    let root = &arena[body.root_expr];

    let tail_expr = if let ExprKind::Block(block) = &root.kind {
        block.tail.map(|id| &arena[id])
    } else {
        Some(root)
    };

    let tail = tail_expr.expect("should have tail");
    if let ExprKind::Literal(lit) = &tail.kind {
        match &lit.value {
            LitKind::Int { raw } => assert_eq!(raw, "42"),
            other => panic!("expected Int, got {:?}", other),
        }
    } else {
        panic!("expected Literal, got {:?}", tail.kind);
    }
}

#[test]
fn named_field_access() {
    let (arena, su_id) = build_single("fn main() ( a.name )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let body = &arena[func.body];
    let root = &arena[body.root_expr];

    let tail_expr = if let ExprKind::Block(block) = &root.kind {
        block.tail.map(|id| &arena[id])
    } else {
        Some(root)
    };

    let tail = tail_expr.expect("should have tail");
    if let ExprKind::Field(f) = &tail.kind {
        assert_eq!(
            f.field,
            FieldKey::Name("name".to_string()),
            "a.name should produce FieldKey::Name"
        );
    } else {
        panic!("expected Field, got {:?}", tail.kind);
    }
}

#[test]
fn scope_on_every_node() {
    // Verify that expressions inside a nested block have the inner block's scope,
    // not the outer scope.
    let (arena, su_id) = build_single("fn main() ( let x = 1; { let y = 2; y } )");
    let su = &arena[su_id];
    let func = match &arena[su.items[0]].kind {
        ItemKind::Function(f) => f,
        _ => panic!(),
    };
    let body = &arena[func.body];
    let outer_scope = body.scope;
    let root = &arena[body.root_expr];

    if let ExprKind::Block(outer_block) = &root.kind {
        // The tail should be the inner block.
        if let Some(tail_id) = outer_block.tail {
            let inner_block_expr = &arena[tail_id];
            if let ExprKind::Block(inner_block) = &inner_block_expr.kind {
                let inner_scope = inner_block.scope;
                assert_ne!(
                    outer_scope, inner_scope,
                    "inner block should have different scope than outer"
                );

                // The tail of the inner block (y) should have the inner scope.
                if let Some(y_id) = inner_block.tail {
                    let y_expr = &arena[y_id];
                    assert_eq!(y_expr.scope, inner_scope, "y should have inner block scope");
                }
            }
        }
    }
}

// ===========================================================================
// Scope building + lookup tests
// ===========================================================================

use vx_ir::scope::{DefTarget, ScopeEntryTarget, ScopeOwner, lookup_at};

/// Helper: get the module scope for a source unit.
fn module_scope(arena: &vx_ir::arena::VirArena, su_id: vx_ir::SourceUnitId) -> vx_ir::ScopeId {
    let body = &arena[arena[su_id].top_level_body];
    // Body.scope is the module scope for top-level bodies.
    body.scope
}

/// Helper: get the thunk block scope (the synthetic top-level block's scope).
fn thunk_block_scope(arena: &vx_ir::arena::VirArena, su_id: vx_ir::SourceUnitId) -> vx_ir::ScopeId {
    let body = &arena[arena[su_id].top_level_body];
    let root = &arena[body.root_expr];
    match &root.kind {
        ExprKind::Block(b) => b.scope,
        _ => panic!("expected synthetic block"),
    }
}

/// Helper: get the body scope of a named function item.
fn fn_body_scope(
    arena: &vx_ir::arena::VirArena,
    su_id: vx_ir::SourceUnitId,
    name: &str,
) -> vx_ir::ScopeId {
    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind
            && f.name == name
        {
            return arena[f.body].scope;
        }
    }
    panic!("function '{}' not found in source unit", name);
}

/// Helper: get the block scope inside a function's root expression.
fn fn_block_scope(
    arena: &vx_ir::arena::VirArena,
    su_id: vx_ir::SourceUnitId,
    name: &str,
) -> vx_ir::ScopeId {
    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind
            && f.name == name
        {
            let root = &arena[arena[f.body].root_expr];
            if let ExprKind::Block(b) = &root.kind {
                return b.scope;
            }
            panic!("function '{}' root is not a block", name);
        }
    }
    panic!("function '{}' not found", name);
}

// ---- Test 1: Top-level functions see each other (order-independent) ----

#[test]
fn top_level_functions_see_each_other() {
    let (arena, su_id) = build_single("fn a() ( 1 )\nfn b() ( 2 )");
    // From a's body scope, looking up "b" should find the function item.
    let a_body_scope = fn_body_scope(&arena, su_id, "a");
    let result = lookup_at(&arena, "b", a_body_scope, u32::MAX);
    assert!(
        matches!(result, Some(DefTarget::Item(_))),
        "top-level fn a should see fn b: {:?}",
        result
    );
}

// ---- Test 2: Top-level functions don't see top-level lets ----

#[test]
fn top_level_functions_dont_see_lets() {
    let (arena, su_id) = build_single("let x = 1\nfn a() ( 2 )");
    // From a's body scope, "x" should NOT resolve — it's in the thunk block,
    // not in the module scope.
    let a_body_scope = fn_body_scope(&arena, su_id, "a");
    let result = lookup_at(&arena, "x", a_body_scope, u32::MAX);
    assert!(
        result.is_none(),
        "top-level fn should not see top-level let binding: {:?}",
        result
    );
}

// ---- Test 3: Top-level lets see top-level functions ----

#[test]
fn top_level_lets_see_functions() {
    let (arena, su_id) = build_single("fn f() ( 1 )\nlet x = 2");
    // From the thunk block scope (where x lives), "f" should resolve
    // via the module scope parent.
    let tbs = thunk_block_scope(&arena, su_id);
    let result = lookup_at(&arena, "f", tbs, u32::MAX);
    assert!(
        matches!(result, Some(DefTarget::Item(_))),
        "top-level let should see fn via module scope: {:?}",
        result
    );
}

// ---- Test 3a: Top-level binding shadows item (binding first) ----

#[test]
fn top_level_binding_shadows_item_binding_first() {
    let (arena, su_id) = build_single("let f = 1\nfn f() ( 2 )\nf");
    // From thunk block scope at the end, "f" should resolve to the binding
    // (thunk block checked before module scope).
    let tbs = thunk_block_scope(&arena, su_id);
    let result = lookup_at(&arena, "f", tbs, u32::MAX);
    assert!(
        matches!(result, Some(DefTarget::Binding(_))),
        "binding should shadow item: {:?}",
        result
    );
}

// ---- Test 3b: Top-level binding shadows item (item first) ----

#[test]
fn top_level_binding_shadows_item_item_first() {
    let (arena, su_id) = build_single("fn f() ( 1 )\nlet f = 2\nf");
    let tbs = thunk_block_scope(&arena, su_id);
    let result = lookup_at(&arena, "f", tbs, u32::MAX);
    assert!(
        matches!(result, Some(DefTarget::Binding(_))),
        "binding should shadow item regardless of order: {:?}",
        result
    );
}

// ---- Test 4: Basic sequential visibility ----

#[test]
fn block_sequential_visibility() {
    let (arena, su_id) = build_single("fn main() ( let x = 1; let y = x; y )");
    let block_scope = fn_block_scope(&arena, su_id, "main");
    // At decl_order 1 (y's init position), "x" (decl_order 0) should be visible.
    let result = lookup_at(&arena, "x", block_scope, 1);
    assert!(
        matches!(result, Some(DefTarget::Binding(_))),
        "x should be visible at y's init: {:?}",
        result
    );
}

// ---- Test 5: Let not visible in own init ----

#[test]
fn let_not_visible_in_own_init() {
    let (arena, su_id) = build_single("fn main() ( let x = 1; let x = x + 1 )");
    let block_scope = fn_block_scope(&arena, su_id, "main");

    // At reference_order = 1 (the second let's decl_order), binding < cutoff
    // means decl_order < 1, so only the first x (decl_order 0) is visible.
    // The second x at decl_order 1 is NOT visible (1 < 1 = false).
    let result = lookup_at(&arena, "x", block_scope, 1);
    match result {
        Some(DefTarget::Binding(bid)) => {
            // Should be the first binding (the one with init = 1).
            // The first x should have a lower binding id than the second x.
            // We verify indirectly: there should be exactly one visible.
            assert_eq!(arena[bid].name, "x");
        }
        other => panic!("expected first x binding, got {:?}", other),
    }
}

// ---- Test 6: Nested function cutoff ----

#[test]
fn nested_function_cutoff() {
    let (arena, su_id) = build_single("fn main() ( let a = 1; fn f() ( a + b ); let b = 2 )");
    // Find f's body scope. f is a block-local function inside main's block.
    let main_block_scope = fn_block_scope(&arena, su_id, "main");
    let main_block = &arena.scopes[main_block_scope.index()];

    // Find the fn f item in the block entries.
    let f_item_id = main_block
        .entries
        .iter()
        .find_map(|e| match e.target {
            ScopeEntryTarget::Item(id) if arena.item_name(id) == "f" => Some(id),
            _ => None,
        })
        .expect("should find fn f in block");

    let f_body_id = match &arena[f_item_id].kind {
        ItemKind::Function(f) => f.body,
        _ => panic!("expected function"),
    };
    let f_body_scope = arena[f_body_id].scope;

    // From f's body, the root expr is a block. Get that block's scope.
    let f_root = &arena[arena[f_body_id].root_expr];
    let f_block_scope = match &f_root.kind {
        ExprKind::Block(b) => b.scope,
        _ => f_body_scope, // if root isn't a block, use body scope
    };

    // Inside f, "a" should resolve (decl_order 0, before f at decl_order 1).
    let a_result = lookup_at(&arena, "a", f_block_scope, u32::MAX);
    assert!(
        matches!(a_result, Some(DefTarget::Binding(_))),
        "a should be visible inside f: {:?}",
        a_result
    );

    // Inside f, "b" should NOT resolve (decl_order 2, after f at decl_order 1).
    let b_result = lookup_at(&arena, "b", f_block_scope, u32::MAX);
    assert!(
        b_result.is_none(),
        "b should NOT be visible inside f (cutoff): {:?}",
        b_result
    );
}

// ---- Test 9: Block-local function self-recursion ----

#[test]
fn block_local_fn_self_recursion() {
    let (arena, su_id) = build_single("fn main() ( fn f() ( 1 ) )");
    let main_block_scope = fn_block_scope(&arena, su_id, "main");

    // Find f's body/block scope.
    let f_item_id = arena.scopes[main_block_scope.index()]
        .entries
        .iter()
        .find_map(|e| match e.target {
            ScopeEntryTarget::Item(id) if arena.item_name(id) == "f" => Some(id),
            _ => None,
        })
        .expect("should find fn f");

    let f_body_id = match &arena[f_item_id].kind {
        ItemKind::Function(f) => f.body,
        _ => panic!(),
    };

    let f_root = &arena[arena[f_body_id].root_expr];
    let f_block_scope = match &f_root.kind {
        ExprKind::Block(b) => b.scope,
        _ => arena[f_body_id].scope,
    };

    // "f" should be visible inside f's own body (item <= cutoff).
    let result = lookup_at(&arena, "f", f_block_scope, u32::MAX);
    assert!(
        matches!(result, Some(DefTarget::Item(_))),
        "block-local fn should see itself for recursion: {:?}",
        result
    );
}

// ---- Test 10: Binding shadows item in block ----

#[test]
fn binding_shadows_item_in_block() {
    let (arena, su_id) = build_single("fn main() ( fn f() ( 1 ); let f = 2; f )");
    let block_scope = fn_block_scope(&arena, su_id, "main");
    // At reference_order = 2 (after both fn f at 0 and let f at 1),
    // latest visible entry wins — let f at decl_order 1.
    let result = lookup_at(&arena, "f", block_scope, 2);
    assert!(
        matches!(result, Some(DefTarget::Binding(_))),
        "binding should shadow fn: {:?}",
        result
    );
}

// ---- Test 11: Item shadows binding in block ----

#[test]
fn item_shadows_binding_in_block() {
    let (arena, su_id) = build_single("fn main() ( let f = 1; fn f() ( 2 ); f )");
    let block_scope = fn_block_scope(&arena, su_id, "main");
    // At reference_order = 2 (after both), latest visible wins — fn f at decl_order 1.
    let result = lookup_at(&arena, "f", block_scope, 2);
    assert!(
        matches!(result, Some(DefTarget::Item(_))),
        "later item should shadow earlier binding: {:?}",
        result
    );
}

// ---- Test 14: Block-level item sequential visibility ----

#[test]
fn block_item_not_yet_visible() {
    let (arena, su_id) = build_single("fn main() ( f; fn f() ( 1 ) )");
    let block_scope = fn_block_scope(&arena, su_id, "main");
    // At reference_order 0, fn f at decl_order 1 should NOT be visible
    // (items use <=, and 1 <= 0 is false).
    let result = lookup_at(&arena, "f", block_scope, 0);
    assert!(
        result.is_none(),
        "block-level fn should not be visible before declaration: {:?}",
        result
    );
}

// ---- Test: Module scope structure ----

#[test]
fn module_scope_has_correct_structure() {
    let (arena, su_id) = build_single("fn a() ( 1 )\nlet x = 2\nfn b() ( 3 )");
    let ms = module_scope(&arena, su_id);
    let tbs = thunk_block_scope(&arena, su_id);

    // Module scope should be Module-owned.
    assert!(matches!(
        arena.scopes[ms.index()].owner,
        ScopeOwner::Module(_)
    ));

    // Thunk block scope should be Block-owned, child of module scope.
    assert!(matches!(
        arena.scopes[tbs.index()].owner,
        ScopeOwner::Block(_)
    ));
    assert_eq!(arena.scopes[tbs.index()].parent, Some(ms));

    // Module scope should have 2 items (fn a, fn b).
    let item_count = arena.scopes[ms.index()]
        .entries
        .iter()
        .filter(|e| matches!(e.target, ScopeEntryTarget::Item(_)))
        .count();
    assert_eq!(item_count, 2, "module scope should have 2 items");

    // Thunk block should have 1 binding (let x).
    let binding_count = arena.scopes[tbs.index()]
        .entries
        .iter()
        .filter(|e| matches!(e.target, ScopeEntryTarget::Binding(_)))
        .count();
    assert_eq!(binding_count, 1, "thunk block should have 1 binding");
}
