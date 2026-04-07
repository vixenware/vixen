//! Smoke tests for the VIR arena and visitor.

use vx_resolve::{DefId, SourceId, SourceSpan};
use vx_vir::binding::{Binding, BindingKind};
use vx_vir::body::{Body, BodyOwner, Param};
use vx_vir::expr::{BinOp, BinaryExpr, Expr, ExprKind, LitKind, LiteralExpr, NameRefExpr};
use vx_vir::item::{FunctionItem, Item, ItemKind};
use vx_vir::pattern::{Pattern, PatternBinding, PatternKind};
use vx_vir::scope::{Scope, ScopeOwner};
use vx_vir::source_unit::SourceUnit;
use vx_vir::visitor;
use vx_vir::{BodyId, ExprId, VirArena, VirVisitor};

fn dummy_span() -> SourceSpan {
    SourceSpan::new(SourceId(0), rowan::TextRange::default())
}

/// Allocate a body, expr, binding, scope, and item — index by ID, assert round-trip.
#[test]
fn arena_round_trip() {
    let mut arena = VirArena::new();

    // Scope
    let scope_id = arena.alloc_scope(Scope {
        parent: None,
        owner: ScopeOwner::Body(BodyId(0)), // will be body_id below
        bindings: vec![],
    });

    // Binding
    let binding_id = arena.alloc_binding(Binding {
        name: "x".into(),
        name_span: dummy_span(),
        kind: BindingKind::Parameter,
        body: BodyId(0),
        scope: scope_id,
        ty: None,
        mutable: false,
    });

    // Register binding in scope
    arena[scope_id].bindings.push(binding_id);

    // Pattern for param
    let pat_id = arena.alloc_pattern(Pattern {
        span: dummy_span(),
        kind: PatternKind::Binding(PatternBinding {
            name: "x".into(),
            binding: binding_id,
        }),
        ty: None,
    });

    // Literal expression: 42
    let lit_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        kind: ExprKind::Literal(LiteralExpr {
            value: LitKind::Int(42),
        }),
        ty: None,
    });

    // Body
    let body_id = arena.alloc_body(Body {
        span: dummy_span(),
        owner: BodyOwner::Function {
            def_id: DefId::Source {
                source: SourceId(0),
                local: 0,
            },
            name: "add".into(),
        },
        scope: scope_id,
        params: vec![Param {
            binding: binding_id,
            pattern: pat_id,
            ty: None,
            is_keyword: false,
        }],
        root_expr: lit_id,
        parent: None,
        return_ty: None,
        capabilities: vec![],
    });

    // Item
    let item_id = arena.alloc_item(Item {
        span: dummy_span(),
        def_id: DefId::Source {
            source: SourceId(0),
            local: 0,
        },
        kind: ItemKind::Function(FunctionItem {
            name: "add".into(),
            name_span: dummy_span(),
            body: body_id,
            type_id: None,
            attributes: vec![],
        }),
    });

    // Source unit
    let su_id = arena.alloc_source_unit(SourceUnit {
        source: SourceId(0),
        items: vec![item_id],
        top_level_body: body_id,
    });

    // Round-trip checks
    assert_eq!(arena[binding_id].name, "x");
    assert_eq!(arena[body_id].params.len(), 1);
    assert_eq!(
        arena[item_id].def_id,
        DefId::Source {
            source: SourceId(0),
            local: 0
        }
    );
    assert_eq!(arena[su_id].source, SourceId(0));
    assert_eq!(arena[su_id].items.len(), 1);

    // Scope lookup
    let bindings: Vec<_> = arena.bindings_in_scope(scope_id).collect();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].1.name, "x");

    // Source unit lookup
    let found = arena.source_unit_for(SourceId(0));
    assert!(found.is_some());
    assert_eq!(found.unwrap().0, su_id);
}

/// Build a small nested expression tree and walk it with a counting visitor.
#[test]
fn visitor_traversal_count() {
    let mut arena = VirArena::new();

    // Build: x + 1
    // NameRef "x"
    let name_ref_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        kind: ExprKind::NameRef(NameRefExpr {
            name: "x".into(),
            resolved: None,
        }),
        ty: None,
    });

    // Literal 1
    let lit_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        kind: ExprKind::Literal(LiteralExpr {
            value: LitKind::Int(1),
        }),
        ty: None,
    });

    // Binary: name_ref + lit
    let bin_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        kind: ExprKind::Binary(BinaryExpr {
            lhs: name_ref_id,
            op: BinOp::Add,
            op_span: dummy_span(),
            rhs: lit_id,
        }),
        ty: None,
    });

    // Scope + binding for body param
    let scope_id = arena.alloc_scope(Scope {
        parent: None,
        owner: ScopeOwner::Body(BodyId(0)),
        bindings: vec![],
    });

    let binding_id = arena.alloc_binding(Binding {
        name: "x".into(),
        name_span: dummy_span(),
        kind: BindingKind::Parameter,
        body: BodyId(0),
        scope: scope_id,
        ty: None,
        mutable: false,
    });

    let pat_id = arena.alloc_pattern(Pattern {
        span: dummy_span(),
        kind: PatternKind::Binding(PatternBinding {
            name: "x".into(),
            binding: binding_id,
        }),
        ty: None,
    });

    let body_id = arena.alloc_body(Body {
        span: dummy_span(),
        owner: BodyOwner::Function {
            def_id: DefId::Source {
                source: SourceId(0),
                local: 0,
            },
            name: "add".into(),
        },
        scope: scope_id,
        params: vec![Param {
            binding: binding_id,
            pattern: pat_id,
            ty: None,
            is_keyword: false,
        }],
        root_expr: bin_id,
        parent: None,
        return_ty: None,
        capabilities: vec![],
    });

    // Count visits
    struct Counter {
        exprs: u32,
        bindings: u32,
        patterns: u32,
    }

    impl VirVisitor for Counter {
        fn visit_expr(&mut self, arena: &VirArena, id: ExprId) {
            self.exprs += 1;
            visitor::walk_expr(self, arena, id);
        }
        fn visit_binding(&mut self, _arena: &VirArena, _id: vx_vir::BindingId) {
            self.bindings += 1;
        }
        fn visit_pattern(&mut self, arena: &VirArena, id: vx_vir::PatternId) {
            self.patterns += 1;
            visitor::walk_pattern(self, arena, id);
        }
    }

    let mut counter = Counter {
        exprs: 0,
        bindings: 0,
        patterns: 0,
    };

    counter.visit_body(&arena, body_id);

    // Body walk visits: 1 binding (param) + 1 pattern (param) + 3 exprs (binary + 2 children)
    assert_eq!(counter.exprs, 3, "should visit binary + lhs + rhs");
    assert_eq!(counter.bindings, 2, "param binding + pattern binding");
    assert_eq!(counter.patterns, 1, "param pattern");
}
