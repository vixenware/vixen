//! Smoke tests for the VIR arena and visitor.

use vx_resolve::{DefId, SourceId, SourceSpan};
use vx_vir::binding::{Binding, BindingKind};
use vx_vir::body::{Body, BodyOwner, Param};
use vx_vir::expr::{BinOp, BinaryExpr, Expr, ExprKind, LitKind, LiteralExpr, NameRefExpr};
use vx_vir::item::{FunctionItem, Item, ItemKind};
use vx_vir::pattern::{Pattern, PatternBinding, PatternKind};
use vx_vir::scope::{Scope, ScopeEntry, ScopeEntryTarget, ScopeOwner};
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
        owner: ScopeOwner::Body(BodyId(0)),
        entries: vec![],
        promoted_variants: vec![],
        visibility_cutoff: None,
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
    arena[scope_id].entries.push(ScopeEntry {
        target: ScopeEntryTarget::Binding(binding_id),
        decl_order: 0,
    });

    // Pattern for param
    let pat_id = arena.alloc_pattern(Pattern {
        span: dummy_span(),
        body: BodyId(0),
        scope: scope_id,
        kind: PatternKind::Binding(PatternBinding {
            name: "x".into(),
            binding: binding_id,
        }),
        ty: None,
    });

    // Literal expression: 42
    let lit_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        body: BodyId(0),
        scope: scope_id,
        kind: ExprKind::Literal(LiteralExpr {
            value: LitKind::Int { raw: "42".into() },
        }),
        ty: None,
    });

    // We need an item_id first to reference in BodyOwner::Function.
    // Allocate a placeholder item, then body, then fix up the item.
    let item_id = arena.alloc_item(Item {
        span: dummy_span(),
        def_id: DefId::Source {
            source: SourceId(0),
            local: 0,
        },
        kind: ItemKind::Function(FunctionItem {
            name: "add".into(),
            name_span: dummy_span(),
            body: BodyId(0), // placeholder, fixed below
            type_id: None,
            attributes: vec![],
        }),
    });

    // Body
    let body_id = arena.alloc_body(Body {
        span: dummy_span(),
        owner: BodyOwner::Function { item: item_id },
        scope: scope_id,
        params: vec![Param {
            pattern: pat_id,
            ty: None,
            is_keyword: false,
        }],
        root_expr: lit_id,
        parent: None,
        return_ty: None,
        capabilities: vec![],
    });

    // Fix up item's body reference
    if let ItemKind::Function(ref mut f) = arena[item_id].kind {
        f.body = body_id;
    }

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

    // Scope + binding for body param (allocated first so exprs can reference it)
    let scope_id = arena.alloc_scope(Scope {
        parent: None,
        owner: ScopeOwner::Body(BodyId(0)),
        entries: vec![],
        promoted_variants: vec![],
        visibility_cutoff: None,
    });

    // Build: x + 1
    let name_ref_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        body: BodyId(0),
        scope: scope_id,
        kind: ExprKind::NameRef(NameRefExpr {
            name: "x".into(),
            resolved: None,
        }),
        ty: None,
    });

    let lit_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        body: BodyId(0),
        scope: scope_id,
        kind: ExprKind::Literal(LiteralExpr {
            value: LitKind::Int { raw: "1".into() },
        }),
        ty: None,
    });

    let bin_id = arena.alloc_expr(Expr {
        span: dummy_span(),
        body: BodyId(0),
        scope: scope_id,
        kind: ExprKind::Binary(BinaryExpr {
            lhs: name_ref_id,
            op: BinOp::Add,
            op_span: dummy_span(),
            rhs: lit_id,
        }),
        ty: None,
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

    arena[scope_id].entries.push(ScopeEntry {
        target: ScopeEntryTarget::Binding(binding_id),
        decl_order: 0,
    });

    let pat_id = arena.alloc_pattern(Pattern {
        span: dummy_span(),
        body: BodyId(0),
        scope: scope_id,
        kind: PatternKind::Binding(PatternBinding {
            name: "x".into(),
            binding: binding_id,
        }),
        ty: None,
    });

    // Item placeholder for BodyOwner::Function
    let item_id = arena.alloc_item(Item {
        span: dummy_span(),
        def_id: DefId::Source {
            source: SourceId(0),
            local: 0,
        },
        kind: ItemKind::Function(FunctionItem {
            name: "add".into(),
            name_span: dummy_span(),
            body: BodyId(0),
            type_id: None,
            attributes: vec![],
        }),
    });

    let body_id = arena.alloc_body(Body {
        span: dummy_span(),
        owner: BodyOwner::Function { item: item_id },
        scope: scope_id,
        params: vec![Param {
            pattern: pat_id,
            ty: None,
            is_keyword: false,
        }],
        root_expr: bin_id,
        parent: None,
        return_ty: None,
        capabilities: vec![],
    });

    if let ItemKind::Function(ref mut f) = arena[item_id].kind {
        f.body = body_id;
    }

    // Count visits
    struct Counter {
        exprs: u32,
        bindings: u32,
        patterns: u32,
        scopes: u32,
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
        fn visit_scope(&mut self, arena: &VirArena, id: vx_vir::ScopeId) {
            self.scopes += 1;
            visitor::walk_scope(self, arena, id);
        }
    }

    let mut counter = Counter {
        exprs: 0,
        bindings: 0,
        patterns: 0,
        scopes: 0,
    };

    counter.visit_body(&arena, body_id);

    // walk_body visits: scope (1 binding in it) + param pattern (1 binding) + root expr (3: binary+lhs+rhs)
    assert_eq!(counter.exprs, 3, "binary + lhs + rhs");
    assert_eq!(counter.patterns, 1, "param pattern");
    assert_eq!(counter.bindings, 2, "scope binding + pattern binding");
    assert_eq!(counter.scopes, 1, "body scope");
}
