//! CST-to-VIR builder.
//!
//! Walks the parser's concrete syntax tree and produces a structurally complete
//! [`VirArena`] with explicit parent-child relationships. The builder populates
//! all structural fields (kind, span, body, scope, children, bindings) but
//! leaves resolution and type fields as `None` — those are filled by later passes.
//!
//! ## Invariants
//!
//! - **Paren elimination**: `ParenExpr` and `ParenPattern` are unwrapped; the VIR
//!   node carries the inner expression/pattern's span.
//! - **No call flattening**: curried calls `f(a)(b)` produce nested `CallExpr` nodes.
//! - **Raw literals**: `LitKind::Int/Float/String` store raw token text, not parsed values.
//! - **Error recovery**: the builder emits a VIR node for every element child in
//!   every list context (blocks, arms, fields, params, args, type refs, patterns).
//!   Missing children become `Error` variants with the parent's span. The one
//!   exception is attribute internals, where non-`ATTRIBUTE_ENTRY` children are
//!   skipped as structural punctuation.
//! - **Synthetic blocks**: the top-level body's root expression is a synthetic block
//!   with `synthetic: true` and a span covering the entire file.

use crate::arena::VirArena;
use crate::binding::{Binding, BindingKind};
use crate::body::{Body, BodyOwner, Param};
use crate::expr::{
    self, ArrayElement, BinOp, BinaryExpr as VirBinaryExpr, BlockExpr as VirBlockExpr,
    CallExpr as VirCallExpr, ClosureExpr as VirClosureExpr, Expr, ExprKind,
    FailExpr as VirFailExpr, FallibleExpr as VirFallibleExpr, FieldExpr as VirFieldExpr, FieldKey,
    IfExpr as VirIfExpr, IndexExpr as VirIndexExpr, InvokeExpr as VirInvokeExpr, LitKind,
    LiteralExpr as VirLiteralExpr, MapElement, MapEntry as VirMapEntry, MapExpr as VirMapExpr,
    MapKey, MatchArm as VirMatchArm, MatchExpr as VirMatchExpr, NameRefExpr as VirNameRefExpr,
    PartialAppExpr as VirPartialAppExpr, PartialArg, PathLiteralExpr as VirPathLiteralExpr,
    PrefixExpr as VirPrefixExpr, PrefixOp, RangeExpr as VirRangeExpr, RecordExpr as VirRecordExpr,
    RecordField, StringPart, StructLiteralExpr as VirStructLiteralExpr, StructLiteralField,
    TupleExpr as VirTupleExpr, TypeOpExpr as VirTypeOpExpr, WhereExpr as VirWhereExpr,
    WithExpr as VirWithExpr,
};
use crate::expr::{Arg, InterpolatedStringExpr as VirInterpolatedStringExpr, UpdateField};
use crate::item::{
    Attribute as VirAttribute, AttributeEntry as VirAttributeEntry, EnumItem, FieldDecl,
    FunctionItem, Item, ItemKind, NamespaceItem, NewtypeItem, StructItem, TypeAliasItem,
    VariantDecl as VirVariantDecl,
};
use crate::pattern::{
    ArrayPattern as VirArrayPattern, ArrayRestPattern as VirArrayRestPattern, AtPattern,
    OrPattern as VirOrPattern, Pattern, PatternBinding, PatternKind,
    PatternLiteral as VirPatternLiteral, StructPattern as VirStructPattern,
    StructPatternField as VirStructPatternField, TuplePattern as VirTuplePattern,
    VariantPattern as VirVariantPattern,
};
use crate::scope::{Scope, ScopeEntry, ScopeEntryTarget, ScopeOwner};
use crate::source_unit::SourceUnit;
use crate::stmt::{
    AssignStmt, CompoundUpdateStmt, FieldAssignStmt, LetStmt, ReturnStmt, Stmt, StmtKind,
};
use crate::type_ref::{
    FunctionTypeRef, OptionalTypeRef, PathTypeRef, SumTypeRef, TupleTypeRef, TypeRef, TypeRefKind,
};
use crate::{BodyId, ExprId, ItemId, ScopeId, SourceUnitId, TypeRefId};
use vx_parser::{self, AstNode, SourceId, SourceSpan, SyntaxKind};

use crate::def::DefId;

/// The result of building VIR from one or more source files.
#[derive(Debug)]
pub struct VirProgram {
    /// The shared arena containing all VIR nodes across all source files.
    pub arena: VirArena,
    /// One entry per source file, in the order they were built.
    pub source_units: Vec<SourceUnitId>,
}

/// Builds a VIR arena from parsed CST source files.
pub struct VirBuilder {
    arena: VirArena,
    source_id: SourceId,
    body_stack: Vec<BodyId>,
    scope_stack: Vec<ScopeId>,
    /// Counter for generating unique local DefId values per source file.
    next_local_def: u32,
    /// The decl_order to use when registering bindings from let-statement
    /// patterns into the current block scope. Set by `build_stmt_with_order`
    /// before building the let pattern. For non-let contexts (parameters,
    /// match arms), binding registration uses `entries.len()` as order.
    current_let_decl_order: Option<u32>,
    /// The current statement's decl_order in the enclosing block scope.
    /// Used to set `visibility_cutoff` on closure body scopes so they
    /// can only see parent bindings declared before the containing statement.
    current_block_stmt_order: Option<u32>,
}

impl VirBuilder {
    fn current_body(&self) -> BodyId {
        *self.body_stack.last().unwrap()
    }

    fn current_scope(&self) -> ScopeId {
        *self.scope_stack.last().unwrap()
    }

    fn with_body<R>(&mut self, body: BodyId, f: impl FnOnce(&mut Self) -> R) -> R {
        self.body_stack.push(body);
        let result = f(self);
        self.body_stack.pop();
        result
    }

    fn with_scope<R>(&mut self, scope: ScopeId, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scope_stack.push(scope);
        let result = f(self);
        self.scope_stack.pop();
        result
    }

    fn span(&self, node: &vx_parser::SyntaxNode) -> SourceSpan {
        SourceSpan::new(self.source_id, node.text_range())
    }

    fn token_span(&self, token: &vx_parser::SyntaxToken) -> SourceSpan {
        SourceSpan::new(self.source_id, token.text_range())
    }

    /// Register a binding in its scope.
    ///
    /// If `current_let_decl_order` is set (i.e., we're inside a let-statement
    /// pattern), use that as the decl_order. Otherwise, use the given `decl_order`.
    fn register_binding(&mut self, scope: ScopeId, binding_id: crate::BindingId, decl_order: u32) {
        let order = self.current_let_decl_order.unwrap_or(decl_order);
        self.arena[scope].entries.push(ScopeEntry {
            target: ScopeEntryTarget::Binding(binding_id),
            decl_order: order,
        });
    }

    fn error_expr(&mut self, span: SourceSpan) -> ExprId {
        self.arena.alloc_expr(Expr {
            span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Error,
            ty: None,
        })
    }

    fn error_pattern(&mut self, span: SourceSpan) -> crate::PatternId {
        self.arena.alloc_pattern(Pattern {
            span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Error,
            ty: None,
        })
    }

    fn error_type_ref(&mut self, span: SourceSpan) -> TypeRefId {
        self.arena.alloc_type_ref(TypeRef {
            span,
            kind: TypeRefKind::Error,
        })
    }

    /// Generate a DefId for a new item declaration.
    ///
    /// TRANSITIONAL: The builder assigns DefId::Source values during construction
    /// so that items have identity from the start. When the resolver is integrated
    /// (work item 002), the resolver becomes the canonical authority for DefIds
    /// and these builder-assigned IDs may be replaced or reconciled.
    fn next_def_id(&mut self) -> DefId {
        let id = DefId::Source {
            source: self.source_id,
            local: self.next_local_def,
        };
        self.next_local_def += 1;
        id
    }

    /// Legacy compatibility: promote Option enum variants to the parent scope.
    ///
    /// This is a **hardcoded special case**, not a general enum scoping rule.
    /// When/if Vixen gets a proper prelude or import system, this should be
    /// replaced with explicit imports.
    fn maybe_promote_enum_variants_for(&mut self, scope: ScopeId, item_id: ItemId) {
        // Extract variant data first to avoid borrow conflict.
        let variants: Vec<(String, SourceSpan)> = match &self.arena[item_id].kind {
            ItemKind::Enum(e) if e.name == "Option" => e
                .variants
                .iter()
                .map(|v| (v.name.clone(), v.name_span))
                .collect(),
            _ => return,
        };
        for (i, (name, name_span)) in variants.into_iter().enumerate() {
            self.arena[scope]
                .promoted_variants
                .push(crate::scope::PromotedVariant {
                    name,
                    name_span,
                    enum_item: item_id,
                    variant_index: i as u32,
                });
        }
    }

    // -----------------------------------------------------------------------
    // Public entry points
    // -----------------------------------------------------------------------

    /// Build a VIR program from multiple parsed source files.
    pub fn build_program(sources: &[(SourceId, vx_parser::SourceFile)]) -> VirProgram {
        let mut builder = VirBuilder {
            arena: VirArena::new(),
            source_id: SourceId(0),
            body_stack: vec![],
            scope_stack: vec![],
            next_local_def: 0,
            current_let_decl_order: None,
            current_block_stmt_order: None,
        };
        let mut source_units = Vec::new();
        for (source_id, file) in sources {
            let su_id = builder.build_source_unit(*source_id, file);
            source_units.push(su_id);
        }
        VirProgram {
            arena: builder.arena,
            source_units,
        }
    }

    /// Build a single source file into the arena. Returns its SourceUnitId.
    ///
    /// Creates two scope layers:
    /// - **Module scope**: contains item definitions (order-independent). Top-level
    ///   function body scopes parent to this.
    /// - **Thunk block scope**: child of module scope, contains top-level executable
    ///   bindings (sequential). This is the scope of the synthetic top-level block.
    ///
    /// Top-level bindings always shadow top-level items regardless of source order,
    /// because the thunk block is checked before the module scope during parent-chain
    /// lookup. This matches the old scope builder's behavior.
    fn build_source_unit(
        &mut self,
        source_id: SourceId,
        file: &vx_parser::SourceFile,
    ) -> SourceUnitId {
        self.source_id = source_id;
        self.next_local_def = 0;

        // Placeholder SourceUnitId for the module scope owner.
        let placeholder_su = SourceUnitId(self.arena.source_units.len() as u32);

        // Module scope: contains only top-level item definitions (order-independent).
        let module_scope_id = self.arena.alloc_scope(Scope {
            parent: None,
            owner: ScopeOwner::Module(placeholder_su),
            entries: vec![],
            promoted_variants: vec![],
            visibility_cutoff: None,
        });

        // Thunk block scope: child of module scope, holds top-level executable bindings.
        let thunk_block_scope_id = self.arena.alloc_scope(Scope {
            parent: Some(module_scope_id),
            owner: ScopeOwner::Block(ExprId(0)), // fixed up below when block expr is allocated
            entries: vec![],
            promoted_variants: vec![],
            visibility_cutoff: None, // module parent uses order-independent visibility
        });

        // Top-level body. Body.scope = module_scope (the body's own scope, where
        // params would go — there are none for a top-level thunk).
        let body_id = self.arena.alloc_body(Body {
            span: self.span(file.syntax()),
            owner: BodyOwner::TopLevel { source: source_id },
            scope: module_scope_id,
            params: vec![],
            root_expr: ExprId(0), // placeholder, fixed below
            parent: None,
            return_ty: None,
            capabilities: vec![],
            captures: vec![],
        });

        self.body_stack.push(body_id);
        // Push the thunk block scope as current — that's where executable code lives.
        self.scope_stack.push(thunk_block_scope_id);

        // Walk all top-level children in source order, building items and
        // statements. Items go into the module scope (predeclared, order-independent);
        // bindings go into the thunk block scope (sequential).
        let children: Vec<_> = file.syntax().children().collect();
        let mut stmts = Vec::new();
        let mut items = Vec::new();
        let mut stmt_order: u32 = 0;

        // We need to save module_scope_id for item registration.
        for child in &children {
            if let Some(item_node) = vx_parser::Item::cast(child.clone()) {
                let item_id = self.build_item(&item_node);
                items.push(item_id);

                // Register item in module scope (order-independent, but track
                // source position for visibility_cutoff of nested constructs).
                self.arena[module_scope_id].entries.push(ScopeEntry {
                    target: ScopeEntryTarget::Item(item_id),
                    decl_order: stmt_order,
                });

                // Promote Option enum variants to module scope.
                self.maybe_promote_enum_variants_for(module_scope_id, item_id);

                let stmt_id = self.arena.alloc_stmt(Stmt {
                    span: self.span(child),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Item(item_id),
                });
                stmts.push(stmt_id);
            } else if let Some(stmt_node) = vx_parser::Stmt::cast(child.clone()) {
                // Stmt::cast also matches items (via Stmt::FnDecl etc.).
                // Items were already handled above, so skip them.
                match &stmt_node {
                    vx_parser::Stmt::FnDecl(_)
                    | vx_parser::Stmt::StructDecl(_)
                    | vx_parser::Stmt::EnumDecl(_)
                    | vx_parser::Stmt::NewtypeDecl(_)
                    | vx_parser::Stmt::TypeAliasDecl(_)
                    | vx_parser::Stmt::NamespaceDecl(_) => {
                        stmt_order += 1;
                        continue;
                    }
                    _ => {}
                }
                let prev_order = self.current_block_stmt_order;
                self.current_block_stmt_order = Some(stmt_order);
                let stmt_id = self.build_stmt_with_order(&stmt_node, stmt_order);
                self.current_block_stmt_order = prev_order;
                stmts.push(stmt_id);
            } else {
                // Malformed top-level node — emit error statement.
                let stmt_id = self.arena.alloc_stmt(Stmt {
                    span: self.span(child),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Error,
                });
                stmts.push(stmt_id);
            }
            stmt_order += 1;
        }

        // Create the synthetic top-level block.
        let block_expr_id = self.arena.alloc_expr(Expr {
            span: self.span(file.syntax()),
            body: self.current_body(),
            scope: module_scope_id, // block lives in module scope
            kind: ExprKind::Block(VirBlockExpr {
                scope: thunk_block_scope_id,
                stmts,
                tail: None,
                synthetic: true,
            }),
            ty: None,
        });

        // Fix up thunk block scope's owner.
        self.arena[thunk_block_scope_id].owner = ScopeOwner::Block(block_expr_id);

        // Fix up body's root_expr.
        self.arena[body_id].root_expr = block_expr_id;

        self.body_stack.pop();
        self.scope_stack.pop();

        self.arena.alloc_source_unit(SourceUnit {
            source: source_id,
            items,
            top_level_body: body_id,
        })
    }

    // -----------------------------------------------------------------------
    // Items
    // -----------------------------------------------------------------------

    fn build_item(&mut self, item: &vx_parser::Item) -> ItemId {
        match item {
            vx_parser::Item::FnDecl(f) => self.build_function_item(f),
            vx_parser::Item::StructDecl(s) => self.build_struct_item(s),
            vx_parser::Item::EnumDecl(e) => self.build_enum_item(e),
            vx_parser::Item::NewtypeDecl(n) => self.build_newtype_item(n),
            vx_parser::Item::TypeAliasDecl(ta) => self.build_type_alias_item(ta),
            vx_parser::Item::NamespaceDecl(ns) => self.build_namespace_item(ns),
        }
    }

    fn build_function_item(&mut self, f: &vx_parser::FnDecl) -> ItemId {
        let item_span = self.span(f.syntax());
        let def_id = self.next_def_id();
        let name = f.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = f.name().map(|t| self.token_span(&t)).unwrap_or(item_span);
        let attributes = self.build_attributes(f.attributes());
        let type_id = None;
        let type_params = self.extract_type_params(f.syntax());

        // We need to create the item first (for BodyOwner::Function { item }),
        // then create the body.
        let placeholder_body = BodyId(self.arena.bodies.len() as u32);
        let item_id = self.arena.alloc_item(Item {
            span: item_span,
            def_id,
            kind: ItemKind::Function(FunctionItem {
                name,
                name_span,
                type_params,
                body: placeholder_body,
                type_id,
                attributes,
            }),
        });

        let body_id = self.build_function_body(f, item_id);

        // Fix up the item's body reference.
        if let ItemKind::Function(ref mut func) = self.arena[item_id].kind {
            func.body = body_id;
        }

        item_id
    }

    fn build_function_body(&mut self, f: &vx_parser::FnDecl, item_id: ItemId) -> BodyId {
        // parent links to the enclosing function body for capture analysis.
        // Top-level functions have parent: None — they don't capture from the
        // top-level thunk. Only functions nested inside another function/closure
        // body get a parent link.
        let parent_body = self.body_stack.last().copied().filter(|&parent_id| {
            !matches!(self.arena[parent_id].owner, BodyOwner::TopLevel { .. })
        });
        let fn_span = self.span(f.syntax());

        // Scope parent for function bodies:
        // - Nested functions (inside another fn/closure body) → parent = current scope.
        // - Namespace functions → parent = current scope (the namespace scope).
        // - Top-level functions → parent = module scope (can see sibling items,
        //   NOT top-level lets which live in the thunk block scope).
        let parent_scope = if parent_body.is_some() {
            Some(self.current_scope())
        } else {
            let current = self.current_scope();
            match self.arena[current].owner {
                ScopeOwner::Namespace(_) => {
                    // Function inside a namespace: parent to the namespace scope.
                    Some(current)
                }
                _ => {
                    // Top-level function: parent to module scope.
                    // The module scope is the parent of the current scope (thunk block).
                    self.arena[current].parent
                }
            }
        };
        let placeholder_body = BodyId(self.arena.bodies.len() as u32);
        let scope_id = self.arena.alloc_scope(Scope {
            parent: parent_scope,
            owner: ScopeOwner::Body(placeholder_body),
            entries: vec![],
            promoted_variants: vec![],
            // Top-level functions: parent is module scope (order-independent) → no cutoff.
            // Nested functions inside blocks: cutoff = stmt_order so they only
            // see parent bindings declared before them.
            visibility_cutoff: if parent_body.is_some() {
                self.current_block_stmt_order
            } else {
                None
            },
        });

        let body_id = self.arena.alloc_body(Body {
            span: fn_span,
            owner: BodyOwner::Function { item: item_id },
            scope: scope_id,
            params: vec![],       // filled below
            root_expr: ExprId(0), // placeholder
            parent: parent_body,
            return_ty: None, // filled below
            capabilities: self.build_capabilities_from_fn(f),
            captures: vec![],
        });

        self.with_body(body_id, |this| {
            this.with_scope(scope_id, |this| {
                // Build parameters by walking raw children of the parameter lists.
                let mut params = Vec::new();
                let param_clause = f
                    .syntax()
                    .children()
                    .find(|c| c.kind() == SyntaxKind::PARAMETER_CLAUSE);
                if let Some(clause) = &param_clause {
                    // Positional parameters live in PARAMETER_LIST.
                    if let Some(param_list) = clause
                        .children()
                        .find(|c| c.kind() == SyntaxKind::PARAMETER_LIST)
                    {
                        for child in param_list.children() {
                            if let Some(param) = vx_parser::Parameter::cast(child.clone()) {
                                let pat_id = this.build_parameter(&param);
                                let ty = param
                                    .slot_type()
                                    .and_then(|st| st.ty())
                                    .map(|t| this.build_type_ref(&t));
                                params.push(Param {
                                    pattern: pat_id,
                                    ty,
                                    is_keyword: false,
                                    has_default: false,
                                    doc: vx_parser::doc_comment(param.syntax()),
                                });
                            } else {
                                // Malformed parameter — emit error param.
                                let child_span = this.span(&child);
                                params.push(Param {
                                    pattern: this.error_pattern(child_span),
                                    ty: None,
                                    is_keyword: false,
                                    has_default: false,
                                    doc: None,
                                });
                            }
                        }
                    }
                    // Keyword parameters live in KEYWORD_PARAMETER_LIST.
                    // They may either be nested under PARAMETER_CLAUSE (for the
                    // `(positional; key: T)` form) or appear as a direct child
                    // of FN_DECL (for the `(positional) where { key: T }` form).
                    let kw_list = clause
                        .children()
                        .find(|c| c.kind() == SyntaxKind::KEYWORD_PARAMETER_LIST)
                        .or_else(|| {
                            f.syntax()
                                .children()
                                .find(|c| c.kind() == SyntaxKind::KEYWORD_PARAMETER_LIST)
                        });
                    if let Some(kw_list) = kw_list {
                        for child in kw_list.children() {
                            if let Some(kw_param) = vx_parser::KeywordParameter::cast(child.clone())
                            {
                                let pat_id = this.build_keyword_parameter(&kw_param);
                                let ty = kw_param
                                    .slot_type()
                                    .and_then(|st| st.ty())
                                    .map(|t| this.build_type_ref(&t));
                                let has_default = kw_param
                                    .syntax()
                                    .children()
                                    .any(|c| vx_parser::Expr::cast(c).is_some());
                                params.push(Param {
                                    pattern: pat_id,
                                    ty,
                                    is_keyword: true,
                                    has_default,
                                    doc: vx_parser::doc_comment(kw_param.syntax()),
                                });
                            } else {
                                // Malformed keyword parameter — emit error param.
                                let child_span = this.span(&child);
                                params.push(Param {
                                    pattern: this.error_pattern(child_span),
                                    ty: None,
                                    is_keyword: true,
                                    has_default: false,
                                    doc: None,
                                });
                            }
                        }
                    }
                }
                this.arena[body_id].params = params;

                // Return type.
                let return_ty = f
                    .return_type()
                    .and_then(|rt| rt.ty())
                    .map(|t| this.build_type_ref(&t));
                this.arena[body_id].return_ty = return_ty;

                // Body expression.
                let root_expr = if let Some(body_expr) = f.body_expr() {
                    this.build_expr(&body_expr)
                } else {
                    this.error_expr(fn_span)
                };
                this.arena[body_id].root_expr = root_expr;
            });
        });

        body_id
    }

    fn build_parameter(&mut self, param: &vx_parser::Parameter) -> crate::PatternId {
        let param_span = self.span(param.syntax());
        let name = param
            .name()
            .map(|t| t.text().to_string())
            .unwrap_or_default();

        if name == "_" {
            // Wildcard parameter.
            return self.arena.alloc_pattern(Pattern {
                span: param_span,
                body: self.current_body(),
                scope: self.current_scope(),
                kind: PatternKind::Wildcard,
                ty: None,
            });
        }

        let name_span = param
            .name()
            .map(|t| self.token_span(&t))
            .unwrap_or(param_span);

        let binding_id = self.arena.alloc_binding(Binding {
            name: name.clone(),
            name_span,
            kind: BindingKind::Parameter,
            body: self.current_body(),
            scope: self.current_scope(),
            ty: None,
            init_expr: None,
            mutable: false,
        });
        let scope = self.current_scope();
        let order = self.arena[scope].entries.len() as u32;
        self.register_binding(scope, binding_id, order);

        self.arena.alloc_pattern(Pattern {
            span: param_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Binding(PatternBinding {
                name,
                binding: binding_id,
            }),
            ty: None,
        })
    }

    fn build_keyword_parameter(&mut self, param: &vx_parser::KeywordParameter) -> crate::PatternId {
        let param_span = self.span(param.syntax());
        let name = param
            .name()
            .map(|t| t.text().to_string())
            .unwrap_or_default();
        let name_span = param
            .name()
            .map(|t| self.token_span(&t))
            .unwrap_or(param_span);

        let binding_id = self.arena.alloc_binding(Binding {
            name: name.clone(),
            name_span,
            kind: BindingKind::Parameter,
            body: self.current_body(),
            scope: self.current_scope(),
            ty: None,
            init_expr: None,
            mutable: false,
        });
        let scope = self.current_scope();
        let order = self.arena[scope].entries.len() as u32;
        self.register_binding(scope, binding_id, order);

        self.arena.alloc_pattern(Pattern {
            span: param_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Binding(PatternBinding {
                name,
                binding: binding_id,
            }),
            ty: None,
        })
    }

    fn build_struct_item(&mut self, s: &vx_parser::StructDecl) -> ItemId {
        let item_span = self.span(s.syntax());
        let def_id = self.next_def_id();
        let name = s.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = s.name().map(|t| self.token_span(&t)).unwrap_or(item_span);
        let attributes = self.build_attributes(s.attributes());
        let type_params = self.extract_type_params(s.syntax());

        // Iterate raw STRUCT_BODY children so malformed nodes produce error fields.
        let fields: Vec<FieldDecl> = {
            let body_node = s
                .syntax()
                .children()
                .find(|c| c.kind() == SyntaxKind::STRUCT_BODY);
            let mut fields = Vec::new();
            if let Some(body_node) = body_node {
                for child in body_node.children() {
                    if let Some(fd) = vx_parser::FieldDecl::cast(child.clone()) {
                        fields.push(self.build_field_decl(&fd));
                    } else {
                        // Malformed child — emit error field with span.
                        let child_span = self.span(&child);
                        fields.push(FieldDecl {
                            name: String::new(),
                            name_span: child_span,
                            ty: None,
                            has_default: false,
                            default_value: None,
                            attributes: vec![],
                        });
                    }
                }
            }
            fields
        };

        self.arena.alloc_item(Item {
            span: item_span,
            def_id,
            kind: ItemKind::Struct(StructItem {
                name,
                name_span,
                type_id: None,
                type_params,
                fields,
                attributes,
            }),
        })
    }

    fn build_enum_item(&mut self, e: &vx_parser::EnumDecl) -> ItemId {
        let item_span = self.span(e.syntax());
        let def_id = self.next_def_id();
        let name = e.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = e.name().map(|t| self.token_span(&t)).unwrap_or(item_span);
        let attributes = self.build_attributes(e.attributes());
        let type_params = self.extract_type_params(e.syntax());

        // Iterate raw ENUM_BODY children so malformed nodes produce error variants.
        let variants: Vec<VirVariantDecl> = {
            let body_node = e
                .syntax()
                .children()
                .find(|c| c.kind() == SyntaxKind::ENUM_BODY);
            let mut variants = Vec::new();
            if let Some(body_node) = body_node {
                for child in body_node.children() {
                    if let Some(vd) = vx_parser::VariantDecl::cast(child.clone()) {
                        variants.push(self.build_variant_decl(&vd));
                    } else {
                        // Malformed child — emit error variant.
                        let child_span = self.span(&child);
                        variants.push(VirVariantDecl {
                            name: String::new(),
                            name_span: child_span,
                            fields: vec![],
                            attributes: vec![],
                        });
                    }
                }
            }
            variants
        };

        self.arena.alloc_item(Item {
            span: item_span,
            def_id,
            kind: ItemKind::Enum(EnumItem {
                name,
                name_span,
                type_id: None,
                type_params,
                variants,
                attributes,
            }),
        })
    }

    fn build_newtype_item(&mut self, n: &vx_parser::NewtypeDecl) -> ItemId {
        let item_span = self.span(n.syntax());
        let def_id = self.next_def_id();
        let name = n.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = n.name().map(|t| self.token_span(&t)).unwrap_or(item_span);
        let attributes = self.build_attributes(n.attributes());
        let type_params = self.extract_type_params(n.syntax());
        let inner_ty = n.inner_type().map(|t| self.build_type_ref(&t));

        self.arena.alloc_item(Item {
            span: item_span,
            def_id,
            kind: ItemKind::Newtype(NewtypeItem {
                name,
                name_span,
                type_id: None,
                type_params,
                inner_ty,
                attributes,
            }),
        })
    }

    fn build_type_alias_item(&mut self, ta: &vx_parser::TypeAliasDecl) -> ItemId {
        let item_span = self.span(ta.syntax());
        let def_id = self.next_def_id();
        let name = ta.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = ta.name().map(|t| self.token_span(&t)).unwrap_or(item_span);
        let attributes = self.build_attributes(ta.attributes());
        let type_params = self.extract_type_params(ta.syntax());
        let aliased_ty = ta.aliased_type().map(|t| self.build_type_ref(&t));

        self.arena.alloc_item(Item {
            span: item_span,
            def_id,
            kind: ItemKind::TypeAlias(TypeAliasItem {
                name,
                name_span,
                type_id: None,
                type_params,
                aliased_ty,
                attributes,
            }),
        })
    }

    fn build_namespace_item(&mut self, ns: &vx_parser::NamespaceDecl) -> ItemId {
        let item_span = self.span(ns.syntax());
        let def_id = self.next_def_id();
        let name = ns.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = ns.name().map(|t| self.token_span(&t)).unwrap_or(item_span);
        // Extract type params from namespace target (e.g. `namespace Foo[T, U]`).
        let type_params: Vec<String> = ns
            .target()
            .and_then(|target| target.type_args())
            .map(|type_args| {
                type_args
                    .types()
                    .filter_map(|ty| match ty {
                        vx_parser::Type::Path(path) => {
                            let mut segments = path.segments();
                            let name = segments.next()?;
                            segments.next().is_none().then(|| name.text().to_string())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // NamespaceDecl doesn't have a typed attributes() method, so
        // extract the AttributeList from raw syntax.
        let attributes = {
            let attr_list = ns
                .syntax()
                .children()
                .find_map(vx_parser::AttributeList::cast);
            self.build_attributes(attr_list)
        };

        // Placeholder ItemId for namespace — fix up after alloc.
        let placeholder_item = ItemId(self.arena.items.len() as u32);
        let scope_id = self.arena.alloc_scope(Scope {
            parent: Some(self.current_scope()),
            owner: ScopeOwner::Namespace(placeholder_item),
            entries: vec![],
            promoted_variants: vec![],
            visibility_cutoff: None, // namespace parent determines cutoff
        });

        let child_items: Vec<ItemId> = if let Some(body) = ns.body() {
            self.with_scope(scope_id, |this| {
                let mut items = Vec::new();
                // PASS 1: Build all items and predeclare them in the namespace scope.
                let children: Vec<_> = body.syntax().children().collect();
                for (order, child) in (0_u32..).zip(children.iter()) {
                    if let Some(item_node) = vx_parser::Item::cast(child.clone()) {
                        let item_id = this.build_item(&item_node);
                        items.push(item_id);
                        this.arena[scope_id].entries.push(ScopeEntry {
                            target: ScopeEntryTarget::Item(item_id),
                            decl_order: order,
                        });
                        // Promote Option enum variants.
                        this.maybe_promote_enum_variants_for(scope_id, item_id);
                    } else {
                        // Malformed child inside namespace body — emit error item.
                        let child_span = this.span(child);
                        let def_id = this.next_def_id();
                        items.push(this.arena.alloc_item(Item {
                            span: child_span,
                            def_id,
                            kind: ItemKind::Error,
                        }));
                    }
                }
                items
            })
        } else {
            vec![]
        };

        let item_id = self.arena.alloc_item(Item {
            span: item_span,
            def_id,
            kind: ItemKind::Namespace(NamespaceItem {
                name,
                name_span,
                scope: scope_id,
                type_params,
                items: child_items,
                attributes,
            }),
        });

        // Fix up scope owner with actual ItemId.
        self.arena[scope_id].owner = ScopeOwner::Namespace(item_id);

        item_id
    }

    // -----------------------------------------------------------------------
    // Item helpers
    // -----------------------------------------------------------------------

    fn build_field_decl(&mut self, f: &vx_parser::FieldDecl) -> FieldDecl {
        let name = f.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = f
            .name()
            .map(|t| self.token_span(&t))
            .unwrap_or_else(|| self.span(f.syntax()));
        let ty = f
            .slot_type()
            .and_then(|st| st.ty())
            .map(|t| self.build_type_ref(&t));
        let attributes = self.build_attributes(f.attributes());

        // Default value: FieldDecl CST has `name: Type = expr`.
        // The expr child (if any) is the default.
        let default_value = f
            .syntax()
            .children()
            .find_map(vx_parser::Expr::cast)
            .map(|e| self.build_expr(&e));

        FieldDecl {
            name,
            name_span,
            ty,
            has_default: default_value.is_some(),
            default_value,
            attributes,
        }
    }

    fn build_variant_decl(&mut self, v: &vx_parser::VariantDecl) -> VirVariantDecl {
        let name = v.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = v
            .name()
            .map(|t| self.token_span(&t))
            .unwrap_or_else(|| self.span(v.syntax()));
        let attributes = self.build_attributes(v.attributes());
        // Iterate raw STRUCT_BODY children for recovery of malformed fields.
        let fields: Vec<FieldDecl> = {
            let body_node = v
                .syntax()
                .children()
                .find(|c| c.kind() == SyntaxKind::STRUCT_BODY);
            let mut fields = Vec::new();
            if let Some(body_node) = body_node {
                for child in body_node.children() {
                    if let Some(fd) = vx_parser::FieldDecl::cast(child.clone()) {
                        fields.push(self.build_field_decl(&fd));
                    } else {
                        let child_span = self.span(&child);
                        fields.push(FieldDecl {
                            name: String::new(),
                            name_span: child_span,
                            ty: None,
                            has_default: false,
                            default_value: None,
                            attributes: vec![],
                        });
                    }
                }
            }
            fields
        };

        VirVariantDecl {
            name,
            name_span,
            fields,
            attributes,
        }
    }

    fn build_attributes(
        &mut self,
        attr_list: Option<vx_parser::AttributeList>,
    ) -> Vec<VirAttribute> {
        let Some(list) = attr_list else {
            return vec![];
        };
        let mut attrs = Vec::new();
        for child in list.syntax().children() {
            if let Some(attr) = vx_parser::Attribute::cast(child.clone()) {
                let attr_span = self.span(attr.syntax());
                let mut entries = Vec::new();
                for entry_child in attr.syntax().children() {
                    if let Some(entry) = vx_parser::AttributeEntry::cast(entry_child.clone()) {
                        let key = entry
                            .key()
                            .map(|t| t.text().to_string())
                            .unwrap_or_default();
                        let key_span = entry
                            .key()
                            .map(|t| self.token_span(&t))
                            .unwrap_or(attr_span);
                        let value = entry.value_expr().map(|e| self.build_expr(&e));
                        entries.push(VirAttributeEntry {
                            key,
                            key_span,
                            value,
                        });
                    }
                    // Non-ATTRIBUTE_ENTRY children in attributes are structural
                    // (e.g. punctuation) — not worth emitting error entries for.
                }
                attrs.push(VirAttribute {
                    entries,
                    span: attr_span,
                });
            }
            // Non-ATTRIBUTE children in the list are structural — skip.
        }
        attrs
    }

    fn build_capabilities_from_fn(&self, f: &vx_parser::FnDecl) -> Vec<crate::body::CapabilityTag> {
        // CapabilityList is not exported from vx-parser, so work with raw syntax.
        // Look for CAPABILITY_LIST child containing CAPABILITY_TAG nodes with LOWER_IDENT tokens.
        f.syntax()
            .children()
            .find(|c| c.kind() == SyntaxKind::CAPABILITY_LIST)
            .map(|list| {
                list.children()
                    .filter(|c| c.kind() == SyntaxKind::CAPABILITY_TAG)
                    .filter_map(|tag| {
                        tag.children_with_tokens()
                            .find_map(|c| {
                                c.into_token()
                                    .filter(|t| t.kind() == SyntaxKind::LOWER_IDENT)
                            })
                            .map(|t| crate::body::CapabilityTag {
                                name: t.text().to_string(),
                                name_span: self.token_span(&t),
                            })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn extract_type_params(&self, node: &vx_parser::SyntaxNode) -> Vec<String> {
        // TYPE_PARAM_LIST contains UPPER_IDENT tokens for type parameter names.
        node.children()
            .find(|c| c.kind() == SyntaxKind::TYPE_PARAM_LIST)
            .map(|list| {
                list.children_with_tokens()
                    .filter_map(|child| child.into_token())
                    .filter(|tok| tok.kind() == SyntaxKind::UPPER_IDENT)
                    .map(|tok| tok.text().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Expressions
    // -----------------------------------------------------------------------

    fn build_expr(&mut self, expr: &vx_parser::Expr) -> ExprId {
        match expr {
            vx_parser::Expr::Paren(p) => {
                // Paren elimination: unwrap, use inner expression's span.
                if let Some(inner) = p.expr() {
                    self.build_expr(&inner)
                } else {
                    self.error_expr(self.span(p.syntax()))
                }
            }
            vx_parser::Expr::Block(b) => self.build_block_expr(b),
            vx_parser::Expr::Closure(c) => self.build_closure_expr(c),
            vx_parser::Expr::Call(c) => self.build_call_expr(c),
            vx_parser::Expr::Where(w) => self.build_where_expr(w),
            vx_parser::Expr::Invoke(i) => self.build_invoke_expr(i),
            vx_parser::Expr::PartialApp(pa) => self.build_partial_app_expr(pa),
            vx_parser::Expr::Literal(l) => self.build_literal_expr(l),
            vx_parser::Expr::NameRef(nr) => self.build_name_ref_expr(nr),
            vx_parser::Expr::Binary(b) => self.build_binary_expr(b),
            vx_parser::Expr::Prefix(p) => self.build_prefix_expr(p),
            vx_parser::Expr::If(i) => self.build_if_expr(i),
            vx_parser::Expr::Match(m) => self.build_match_expr(m),
            vx_parser::Expr::Tuple(t) => self.build_tuple_expr(t),
            vx_parser::Expr::Array(a) => self.build_array_expr(a),
            vx_parser::Expr::Record(r) => self.build_record_expr(r),
            vx_parser::Expr::Map(m) => self.build_map_expr(m),
            vx_parser::Expr::StructLiteral(sl) => self.build_struct_literal_expr(sl),
            vx_parser::Expr::Field(f) => self.build_field_expr(f),
            vx_parser::Expr::Index(i) => self.build_index_expr(i),
            vx_parser::Expr::Range(r) => self.build_range_expr(r),
            vx_parser::Expr::TypeOp(t) => self.build_type_op_expr(t),
            vx_parser::Expr::With(w) => self.build_with_expr(w),
            vx_parser::Expr::Fallible(f) => self.build_fallible_expr(f),
            vx_parser::Expr::Fail(f) => self.build_fail_expr(f),
            vx_parser::Expr::Bomb(b) => self.build_bomb_expr(b),
            vx_parser::Expr::InterpolatedString(is) => self.build_interpolated_string_expr(is),
            vx_parser::Expr::PathLiteral(pl) => self.build_path_literal_expr(pl),
            vx_parser::Expr::Import(i) => self.build_import_expr(i),
        }
    }

    fn build_block_expr(&mut self, block: &vx_parser::BlockExpr) -> ExprId {
        let block_span = self.span(block.syntax());
        let parent_scope = self.current_scope();

        // Placeholder ExprId for the block — we'll fix up the scope owner after allocation.
        let placeholder_expr = ExprId(self.arena.exprs.len() as u32);
        let scope_id = self.arena.alloc_scope(Scope {
            parent: Some(parent_scope),
            owner: ScopeOwner::Block(placeholder_expr),
            entries: vec![],
            promoted_variants: vec![],
            // Blocks inside other blocks: visibility_cutoff not set here because
            // the block itself is not a declaration — it's an expression. The cutoff
            // is meaningful for function/closure bodies, not for inner blocks.
            // Inner blocks see everything their parent scope sees at the point
            // where the block appears (handled by the parent scope's visibility rules).
            visibility_cutoff: None,
        });

        self.with_scope(scope_id, |this| {
            let mut stmts = Vec::new();

            let children: Vec<_> = block.syntax().children().collect();

            // Determine the tail: the last element child that casts to Expr
            // and is NOT followed by a semicolon token. In Vixen blocks,
            // `( let x = 1; x + 1 )` has `x + 1` as tail (no trailing `;`).
            let tail_node_index = children.last().and_then(|last_child| {
                // Must cast to Expr to be a tail.
                vx_parser::Expr::cast(last_child.clone())?;
                // Check that no semicolon follows this node in the token stream.
                let has_trailing_semi = last_child
                    .next_sibling_or_token()
                    .is_some_and(|t| t.kind() == vx_parser::SyntaxKind::SEMICOLON);
                if has_trailing_semi {
                    None
                } else {
                    Some(children.len() - 1)
                }
            });

            let mut stmt_order: u32 = 0;
            for (i, child) in children.iter().enumerate() {
                if Some(i) == tail_node_index {
                    // This is the tail — skip adding it as a statement.
                    continue;
                }
                let prev_order = this.current_block_stmt_order;
                this.current_block_stmt_order = Some(stmt_order);
                if let Some(item_node) = vx_parser::Item::cast(child.clone()) {
                    let item_id = this.build_item(&item_node);
                    // Block items have sequential visibility — register with decl_order.
                    this.arena[scope_id].entries.push(ScopeEntry {
                        target: ScopeEntryTarget::Item(item_id),
                        decl_order: stmt_order,
                    });

                    let stmt_id = this.arena.alloc_stmt(Stmt {
                        span: this.span(child),
                        body: this.current_body(),
                        scope: this.current_scope(),
                        kind: StmtKind::Item(item_id),
                    });
                    stmts.push(stmt_id);
                } else if vx_parser::Stmt::cast(child.clone()).is_some() {
                    let stmt = vx_parser::Stmt::cast(child.clone()).unwrap();
                    let stmt_id = this.build_stmt_with_order(&stmt, stmt_order);
                    stmts.push(stmt_id);
                } else {
                    // Malformed child node that doesn't cast to Item or Stmt.
                    // Emit an error statement so we never silently skip a CST node.
                    let stmt_id = this.arena.alloc_stmt(Stmt {
                        span: this.span(child),
                        body: this.current_body(),
                        scope: this.current_scope(),
                        kind: StmtKind::Error,
                    });
                    stmts.push(stmt_id);
                }
                this.current_block_stmt_order = prev_order;
                stmt_order += 1;
            }

            // Build the tail expression.
            let tail = tail_node_index.map(|i| {
                let expr = vx_parser::Expr::cast(children[i].clone()).unwrap();
                this.build_expr(&expr)
            });

            let block_expr_id = this.arena.alloc_expr(Expr {
                span: block_span,
                body: this.current_body(),
                scope: parent_scope, // The block node itself lives in the parent scope.
                kind: ExprKind::Block(VirBlockExpr {
                    scope: scope_id,
                    stmts,
                    tail,
                    synthetic: false,
                }),
                ty: None,
            });

            // Fix up scope owner with the actual ExprId.
            this.arena[scope_id].owner = ScopeOwner::Block(block_expr_id);

            block_expr_id
        })
    }

    fn build_closure_expr(&mut self, closure: &vx_parser::ClosureExpr) -> ExprId {
        let closure_span = self.span(closure.syntax());
        let parent_scope = self.current_scope();
        let parent_body = self.current_body();

        // Create body + scope for the closure.
        let placeholder_body = BodyId(self.arena.bodies.len() as u32);
        let scope_id = self.arena.alloc_scope(Scope {
            parent: Some(parent_scope),
            owner: ScopeOwner::Body(placeholder_body),
            entries: vec![],
            promoted_variants: vec![],
            // Closure body scopes inside blocks need a cutoff so they only
            // see parent bindings declared before the containing statement.
            visibility_cutoff: self.current_block_stmt_order,
        });

        let body_id = self.arena.alloc_body(Body {
            span: closure_span,
            owner: BodyOwner::Closure,
            scope: scope_id,
            params: vec![],
            root_expr: ExprId(0), // placeholder
            parent: Some(parent_body),
            return_ty: None,
            capabilities: vec![],
            captures: vec![],
        });

        self.with_body(body_id, |this| {
            this.with_scope(scope_id, |this| {
                // Build closure parameters by walking raw CLOSURE_PARAM_CLAUSE children.
                let mut params: Vec<Param> = Vec::new();
                if let Some(clause) = closure
                    .syntax()
                    .children()
                    .find(|c| c.kind() == SyntaxKind::CLOSURE_PARAM_CLAUSE)
                {
                    for child in clause.children() {
                        if let Some(bp) = vx_parser::BindingPattern::cast(child.clone()) {
                            let pat_id = this.build_binding_pattern_as_param(&bp);
                            params.push(Param {
                                pattern: pat_id,
                                ty: None,
                                is_keyword: false,
                                has_default: false,
                                doc: None,
                            });
                        } else {
                            let child_span = this.span(&child);
                            params.push(Param {
                                pattern: this.error_pattern(child_span),
                                ty: None,
                                is_keyword: false,
                                has_default: false,
                                doc: None,
                            });
                        }
                    }
                }
                this.arena[body_id].params = params;

                // Build body expression.
                let root_expr = if let Some(body_expr) = closure.body() {
                    this.build_expr(&body_expr)
                } else {
                    this.error_expr(closure_span)
                };
                this.arena[body_id].root_expr = root_expr;
            });
        });

        self.arena.alloc_expr(Expr {
            span: closure_span,
            body: parent_body,
            scope: parent_scope,
            kind: ExprKind::Closure(VirClosureExpr { body: body_id }),
            ty: None,
        })
    }

    fn build_binding_pattern_as_param(
        &mut self,
        bp: &vx_parser::BindingPattern,
    ) -> crate::PatternId {
        let pat_span = self.span(bp.syntax());
        let name = bp.name().map(|t| t.text().to_string()).unwrap_or_default();

        if name == "_" {
            return self.arena.alloc_pattern(Pattern {
                span: pat_span,
                body: self.current_body(),
                scope: self.current_scope(),
                kind: PatternKind::Wildcard,
                ty: None,
            });
        }

        let name_span = bp.name().map(|t| self.token_span(&t)).unwrap_or(pat_span);
        let binding_id = self.arena.alloc_binding(Binding {
            name: name.clone(),
            name_span,
            kind: BindingKind::Parameter,
            body: self.current_body(),
            scope: self.current_scope(),
            ty: None,
            init_expr: None,
            mutable: false,
        });
        let scope = self.current_scope();
        let order = self.arena[scope].entries.len() as u32;
        self.register_binding(scope, binding_id, order);

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Binding(PatternBinding {
                name,
                binding: binding_id,
            }),
            ty: None,
        })
    }

    fn build_call_expr(&mut self, call: &vx_parser::CallExpr) -> ExprId {
        let call_span = self.span(call.syntax());

        // Use immediate_callee to preserve nested call structure.
        let callee = call
            .immediate_callee()
            .map(|c| self.build_expr(&c))
            .unwrap_or_else(|| self.error_expr(call_span));

        // Build positional args. The CST CallExpr.arg() returns a single arg
        // (which may be a TupleExpr for multiple args). We need to look at the
        // ARG_LIST child to get individual ARG nodes.
        let args = self.build_call_args(call);

        let type_args: Vec<TypeRefId> = call
            .type_args()
            .map(|ta| {
                let mut args = Vec::new();
                for child in ta.syntax().children() {
                    if let Some(t) = vx_parser::Type::cast(child.clone()) {
                        args.push(self.build_type_ref(&t));
                    } else {
                        args.push(self.error_type_ref(self.span(&child)));
                    }
                }
                args
            })
            .unwrap_or_default();

        self.arena.alloc_expr(Expr {
            span: call_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Call(VirCallExpr {
                callee,
                args,
                type_args,
            }),
            ty: None,
        })
    }

    fn build_call_args(&mut self, call: &vx_parser::CallExpr) -> Vec<Arg> {
        // Walk the raw children of the CALL_EXPR node. The CST structure is:
        //   CALL_EXPR { callee_expr, arg_expr, [TYPE_ARGS] }
        // The callee is the first Expr child (already handled by build_call_expr).
        // The arg is the second Expr child. Multi-arg is encoded as a TupleExpr.
        //
        // We identify the callee by syntax pointer (same node as immediate_callee())
        // so we can skip it precisely and recover error nodes for anything else.
        let callee_node = call.immediate_callee().map(|c| c.syntax().clone());

        let mut args = Vec::new();
        for child in call.syntax().children() {
            // Skip the callee — already built.
            if callee_node.as_ref().is_some_and(|c| *c == child) {
                continue;
            }
            // Skip TYPE_ARGS — already handled separately.
            if child.kind() == SyntaxKind::TYPE_ARGS {
                continue;
            }

            // This child is the arg position. If it casts to Expr, process it.
            if let Some(expr_node) = vx_parser::Expr::cast(child.clone()) {
                match expr_node {
                    // Multi-arg: unwrap the TupleExpr, walk its raw children.
                    vx_parser::Expr::Tuple(tuple) => {
                        for tuple_child in tuple.syntax().children() {
                            if let Some(item) = vx_parser::Expr::cast(tuple_child.clone()) {
                                let arg_span = self.span(item.syntax());
                                let expr = self.build_expr(&item);
                                args.push(Arg {
                                    expr,
                                    span: arg_span,
                                });
                            } else {
                                let child_span = self.span(&tuple_child);
                                args.push(Arg {
                                    expr: self.error_expr(child_span),
                                    span: child_span,
                                });
                            }
                        }
                    }
                    // Single arg.
                    other => {
                        let arg_span = self.span(other.syntax());
                        let expr = self.build_expr(&other);
                        args.push(Arg {
                            expr,
                            span: arg_span,
                        });
                    }
                }
            } else {
                // Malformed child in arg position — emit error arg.
                let child_span = self.span(&child);
                args.push(Arg {
                    expr: self.error_expr(child_span),
                    span: child_span,
                });
            }
        }
        args
    }

    fn build_where_expr(&mut self, w: &vx_parser::WhereExpr) -> ExprId {
        let where_span = self.span(w.syntax());
        let callee = w
            .callee()
            .map(|c| self.build_expr(&c))
            .unwrap_or_else(|| self.error_expr(where_span));
        let keywords = w
            .keywords()
            .map(|k| self.build_expr(&k))
            .unwrap_or_else(|| self.error_expr(where_span));

        self.arena.alloc_expr(Expr {
            span: where_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Where(VirWhereExpr { callee, keywords }),
            ty: None,
        })
    }

    fn build_invoke_expr(&mut self, inv: &vx_parser::InvokeExpr) -> ExprId {
        let inv_span = self.span(inv.syntax());
        let callee = inv
            .callee()
            .map(|c| self.build_expr(&c))
            .unwrap_or_else(|| self.error_expr(inv_span));

        self.arena.alloc_expr(Expr {
            span: inv_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Invoke(VirInvokeExpr { callee }),
            ty: None,
        })
    }

    fn build_partial_app_expr(&mut self, pa: &vx_parser::PartialAppExpr) -> ExprId {
        let pa_span = self.span(pa.syntax());
        let callee = pa
            .callee()
            .map(|c| self.build_expr(&c))
            .unwrap_or_else(|| self.error_expr(pa_span));

        // PartialAppExpr args: look for ARG_LIST → ARG children.
        // ARG nodes with ARG_PLACEHOLDER kind become PartialArg::Placeholder.
        let args: Vec<PartialArg> = pa
            .syntax()
            .children()
            .find(|c| c.kind() == SyntaxKind::ARG_LIST)
            .map(|arg_list| {
                arg_list
                    .children()
                    .filter(|c| {
                        c.kind() == SyntaxKind::ARG || c.kind() == SyntaxKind::ARG_PLACEHOLDER
                    })
                    .map(|arg_node| {
                        if arg_node.kind() == SyntaxKind::ARG_PLACEHOLDER {
                            PartialArg::Placeholder {
                                span: self.span(&arg_node),
                            }
                        } else {
                            let expr = arg_node
                                .children()
                                .find_map(vx_parser::Expr::cast)
                                .map(|e| self.build_expr(&e))
                                .unwrap_or_else(|| self.error_expr(self.span(&arg_node)));
                            PartialArg::Expr(expr)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.arena.alloc_expr(Expr {
            span: pa_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::PartialApp(VirPartialAppExpr { callee, args }),
            ty: None,
        })
    }

    fn build_literal_expr(&mut self, lit: &vx_parser::LiteralExpr) -> ExprId {
        let lit_span = self.span(lit.syntax());
        let kind = if let Some(token) = lit.token() {
            match token.kind() {
                SyntaxKind::INT => LitKind::Int {
                    raw: token.text().to_string(),
                },
                SyntaxKind::FLOAT => LitKind::Float {
                    raw: token.text().to_string(),
                },
                SyntaxKind::STRING => {
                    // Strip surrounding quotes.
                    let text = token.text();
                    let raw = if text.len() >= 2 {
                        text[1..text.len() - 1].to_string()
                    } else {
                        text.to_string()
                    };
                    LitKind::String { raw }
                }
                SyntaxKind::TRUE_KW => LitKind::Bool(true),
                SyntaxKind::FALSE_KW => LitKind::Bool(false),
                _ => {
                    // Unknown literal token — store as raw string.
                    LitKind::String {
                        raw: token.text().to_string(),
                    }
                }
            }
        } else {
            return self.error_expr(lit_span);
        };

        self.arena.alloc_expr(Expr {
            span: lit_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Literal(VirLiteralExpr { value: kind }),
            ty: None,
        })
    }

    fn build_name_ref_expr(&mut self, nr: &vx_parser::NameRefExpr) -> ExprId {
        let nr_span = self.span(nr.syntax());
        let name_token = nr.token();
        let name = name_token
            .as_ref()
            .map(|t| t.text().to_string())
            .unwrap_or_default();
        let name_span = name_token
            .as_ref()
            .map(|t| self.token_span(t))
            .unwrap_or(nr_span);

        self.arena.alloc_expr(Expr {
            span: nr_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::NameRef(VirNameRefExpr {
                name,
                name_span,
                resolved: None,
            }),
            ty: None,
        })
    }

    fn build_binary_expr(&mut self, bin: &vx_parser::BinaryExpr) -> ExprId {
        let bin_span = self.span(bin.syntax());
        let lhs = bin
            .lhs()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(bin_span));
        let rhs = bin
            .rhs()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(bin_span));

        // Extract operator token.
        let op_token = bin
            .syntax()
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .find(|token| {
                matches!(
                    token.kind(),
                    SyntaxKind::PLUS
                        | SyntaxKind::PLUS2
                        | SyntaxKind::MINUS
                        | SyntaxKind::STAR
                        | SyntaxKind::SLASH
                        | SyntaxKind::PERCENT
                        | SyntaxKind::EQ2
                        | SyntaxKind::BANG_EQ
                        | SyntaxKind::LT
                        | SyntaxKind::LT_EQ
                        | SyntaxKind::GT
                        | SyntaxKind::GT_EQ
                        | SyntaxKind::AMP2
                        | SyntaxKind::PIPE2
                        | SyntaxKind::PIPE_GT
                )
            });

        let (op, op_span) = if let Some(token) = op_token {
            let op = match token.kind() {
                SyntaxKind::PLUS => BinOp::Add,
                SyntaxKind::PLUS2 => BinOp::Concat,
                SyntaxKind::MINUS => BinOp::Sub,
                SyntaxKind::STAR => BinOp::Mul,
                SyntaxKind::SLASH => BinOp::Div,
                SyntaxKind::PERCENT => BinOp::Rem,
                SyntaxKind::EQ2 => BinOp::Eq,
                SyntaxKind::BANG_EQ => BinOp::Neq,
                SyntaxKind::LT => BinOp::Lt,
                SyntaxKind::LT_EQ => BinOp::LtEq,
                SyntaxKind::GT => BinOp::Gt,
                SyntaxKind::GT_EQ => BinOp::GtEq,
                SyntaxKind::AMP2 => BinOp::And,
                SyntaxKind::PIPE2 => BinOp::Or,
                SyntaxKind::PIPE_GT => BinOp::Pipe,
                _ => unreachable!(),
            };
            (op, self.token_span(&token))
        } else {
            // Unknown binary operator — default to Add with parent span.
            (BinOp::Add, bin_span)
        };

        self.arena.alloc_expr(Expr {
            span: bin_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Binary(VirBinaryExpr {
                lhs,
                op,
                op_span,
                rhs,
            }),
            ty: None,
        })
    }

    fn build_prefix_expr(&mut self, pre: &vx_parser::PrefixExpr) -> ExprId {
        let pre_span = self.span(pre.syntax());
        let operand = pre
            .expr()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(pre_span));

        let (op, op_span) = if let Some(token) = pre.operator_token() {
            let op = match token.kind() {
                SyntaxKind::BANG => PrefixOp::Not,
                SyntaxKind::MINUS => PrefixOp::Neg,
                _ => PrefixOp::Not, // fallback
            };
            (op, self.token_span(&token))
        } else {
            (PrefixOp::Not, pre_span)
        };

        self.arena.alloc_expr(Expr {
            span: pre_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Prefix(VirPrefixExpr {
                op,
                op_span,
                operand,
            }),
            ty: None,
        })
    }

    fn build_if_expr(&mut self, if_: &vx_parser::IfExpr) -> ExprId {
        let if_span = self.span(if_.syntax());
        let condition = if_
            .condition()
            .map(|c| self.build_expr(&c))
            .unwrap_or_else(|| self.error_expr(if_span));
        let then_branch = if_
            .then_branch()
            .map(|t| self.build_expr(&t))
            .unwrap_or_else(|| self.error_expr(if_span));
        let else_branch = if_.else_branch().map(|e| self.build_expr(&e));

        self.arena.alloc_expr(Expr {
            span: if_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::If(VirIfExpr {
                condition,
                then_branch,
                else_branch,
            }),
            ty: None,
        })
    }

    fn build_match_expr(&mut self, m: &vx_parser::MatchExpr) -> ExprId {
        let match_span = self.span(m.syntax());
        let scrutinee = m
            .scrutinee()
            .map(|s| self.build_expr(&s))
            .unwrap_or_else(|| self.error_expr(match_span));

        // We need the match ExprId for MatchArm scope owners — use placeholder, fix up after.
        let placeholder_match = ExprId(self.arena.exprs.len() as u32);

        // Iterate raw children so we never silently skip a malformed CST node.
        // The scrutinee is the first Expr child — identify it by syntax pointer
        // so only that specific node is skipped. All other children that aren't
        // MatchArm nodes become error arms.
        let scrutinee_node = m.scrutinee().map(|s| s.syntax().clone());
        let mut arms: Vec<VirMatchArm> = Vec::new();
        let mut arm_index: u32 = 0;
        for child in m.syntax().children() {
            // Skip the scrutinee — it was already built above.
            if scrutinee_node.as_ref().is_some_and(|s| *s == child) {
                continue;
            }
            if let Some(arm) = vx_parser::MatchArm::cast(child.clone()) {
                let arm_span = self.span(arm.syntax());
                let arm_scope = self.arena.alloc_scope(Scope {
                    parent: Some(self.current_scope()),
                    owner: ScopeOwner::MatchArm {
                        match_expr: placeholder_match,
                        arm_index,
                    },
                    entries: vec![],
                    promoted_variants: vec![],
                    visibility_cutoff: None,
                });

                let vir_arm = self.with_scope(arm_scope, |this| {
                    let pattern = arm
                        .pattern()
                        .map(|p| this.build_pattern(&p))
                        .unwrap_or_else(|| this.error_pattern(arm_span));
                    let body = arm
                        .expr()
                        .map(|e| this.build_expr(&e))
                        .unwrap_or_else(|| this.error_expr(arm_span));

                    VirMatchArm {
                        pattern,
                        body,
                        scope: arm_scope,
                        span: arm_span,
                    }
                });
                arms.push(vir_arm);
                arm_index += 1;
            } else {
                // Malformed child node — emit an error arm.
                let child_span = self.span(&child);
                let arm_scope = self.arena.alloc_scope(Scope {
                    parent: Some(self.current_scope()),
                    owner: ScopeOwner::MatchArm {
                        match_expr: placeholder_match,
                        arm_index,
                    },
                    entries: vec![],
                    promoted_variants: vec![],
                    visibility_cutoff: None,
                });
                let vir_arm = self.with_scope(arm_scope, |this| VirMatchArm {
                    pattern: this.error_pattern(child_span),
                    body: this.error_expr(child_span),
                    scope: arm_scope,
                    span: child_span,
                });
                arms.push(vir_arm);
                arm_index += 1;
            }
        }

        let match_expr_id = self.arena.alloc_expr(Expr {
            span: match_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Match(VirMatchExpr { scrutinee, arms }),
            ty: None,
        });

        // Fix up match arm scope owners with actual match ExprId.
        if let ExprKind::Match(ref match_expr) = self.arena[match_expr_id].kind {
            let arm_scopes: Vec<ScopeId> = match_expr.arms.iter().map(|a| a.scope).collect();
            for (i, scope_id) in arm_scopes.into_iter().enumerate() {
                self.arena[scope_id].owner = ScopeOwner::MatchArm {
                    match_expr: match_expr_id,
                    arm_index: i as u32,
                };
            }
        }

        match_expr_id
    }

    fn build_tuple_expr(&mut self, t: &vx_parser::TupleExpr) -> ExprId {
        let tuple_span = self.span(t.syntax());
        let mut elements: Vec<ExprId> = Vec::new();
        for child in t.syntax().children() {
            if let Some(e) = vx_parser::Expr::cast(child.clone()) {
                elements.push(self.build_expr(&e));
            } else {
                elements.push(self.error_expr(self.span(&child)));
            }
        }

        self.arena.alloc_expr(Expr {
            span: tuple_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Tuple(VirTupleExpr { elements }),
            ty: None,
        })
    }

    fn build_array_expr(&mut self, a: &vx_parser::ArrayExpr) -> ExprId {
        let arr_span = self.span(a.syntax());
        let mut elements: Vec<ArrayElement> = Vec::new();
        for child in a.syntax().children() {
            if let Some(item) = vx_parser::ArrayItem::cast(child.clone()) {
                match item {
                    vx_parser::ArrayItem::Expr(e) => {
                        elements.push(ArrayElement::Expr(self.build_expr(&e)));
                    }
                    vx_parser::ArrayItem::Spread(s) => {
                        let inner = s
                            .expr()
                            .map(|e| self.build_expr(&e))
                            .unwrap_or_else(|| self.error_expr(self.span(s.syntax())));
                        elements.push(ArrayElement::Spread(inner));
                    }
                }
            } else {
                // Malformed child — emit error element.
                elements.push(ArrayElement::Expr(self.error_expr(self.span(&child))));
            }
        }

        self.arena.alloc_expr(Expr {
            span: arr_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Array(expr::ArrayExpr { elements }),
            ty: None,
        })
    }

    fn build_record_expr(&mut self, r: &vx_parser::RecordExpr) -> ExprId {
        let rec_span = self.span(r.syntax());
        let mut fields: Vec<RecordField> = Vec::new();
        for child in r.syntax().children() {
            if let Some(entry) = vx_parser::MapEntry::cast(child.clone()) {
                let name = entry
                    .name()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                let value = entry
                    .value()
                    .map(|e| self.build_expr(&e))
                    .unwrap_or_else(|| {
                        // Shorthand: `{ x }` means `{ x: x }`.
                        if !name.is_empty() {
                            let span = entry
                                .name()
                                .map(|t| self.token_span(&t))
                                .unwrap_or_else(|| self.span(entry.syntax()));
                            self.arena.alloc_expr(Expr {
                                span,
                                body: self.current_body(),
                                scope: self.current_scope(),
                                kind: ExprKind::NameRef(VirNameRefExpr {
                                    name: name.clone(),
                                    name_span: span,
                                    resolved: None,
                                }),
                                ty: None,
                            })
                        } else {
                            self.error_expr(self.span(entry.syntax()))
                        }
                    });
                fields.push(RecordField {
                    name,
                    name_span: entry
                        .name()
                        .map(|t| self.token_span(&t))
                        .unwrap_or_else(|| self.span(entry.syntax())),
                    value,
                });
            } else {
                // Malformed child — emit error field.
                let child_span = self.span(&child);
                fields.push(RecordField {
                    name: String::new(),
                    name_span: child_span,
                    value: self.error_expr(child_span),
                });
            }
        }

        self.arena.alloc_expr(Expr {
            span: rec_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Record(VirRecordExpr { fields }),
            ty: None,
        })
    }

    fn build_map_expr(&mut self, m: &vx_parser::MapExpr) -> ExprId {
        let map_span = self.span(m.syntax());
        let mut entries: Vec<MapElement> = Vec::new();
        for child in m.syntax().children() {
            if let Some(entry) = vx_parser::MapEntry::cast(child.clone()) {
                if let Some(spread) = entry.spread() {
                    let inner = spread
                        .expr()
                        .map(|e| self.build_expr(&e))
                        .unwrap_or_else(|| self.error_expr(self.span(spread.syntax())));
                    entries.push(MapElement::Spread(inner));
                } else {
                    let entry_span = self.span(entry.syntax());
                    let key = if let Some(name_tok) = entry.name() {
                        MapKey::Name {
                            name: name_tok.text().to_string(),
                            span: self.token_span(&name_tok),
                        }
                    } else if let Some(key_expr) = entry.key() {
                        MapKey::Expr(self.build_expr(&key_expr))
                    } else {
                        MapKey::Name {
                            name: String::new(),
                            span: entry_span,
                        }
                    };
                    let value = entry
                        .value()
                        .map(|e| self.build_expr(&e))
                        .unwrap_or_else(|| self.error_expr(entry_span));
                    entries.push(MapElement::Entry(VirMapEntry {
                        key,
                        value,
                        span: entry_span,
                    }));
                }
            } else {
                // Malformed child — emit error entry.
                let child_span = self.span(&child);
                entries.push(MapElement::Entry(VirMapEntry {
                    key: MapKey::Name {
                        name: String::new(),
                        span: child_span,
                    },
                    value: self.error_expr(child_span),
                    span: child_span,
                }));
            }
        }

        self.arena.alloc_expr(Expr {
            span: map_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Map(VirMapExpr { entries }),
            ty: None,
        })
    }

    fn build_struct_literal_expr(&mut self, sl: &vx_parser::StructLiteralExpr) -> ExprId {
        let sl_span = self.span(sl.syntax());
        let name = sl.name().map(|t| t.text().to_string()).unwrap_or_default();

        // Find the STRUCT_LITERAL_BODY child, then iterate its raw children.
        let body_node = sl
            .syntax()
            .children()
            .find(|c| c.kind() == SyntaxKind::STRUCT_LITERAL_BODY);
        let mut fields: Vec<StructLiteralField> = Vec::new();
        if let Some(body_node) = body_node {
            for child in body_node.children() {
                if let Some(f) = vx_parser::StructLiteralField::cast(child.clone()) {
                    let fname = f.name().map(|t| t.text().to_string()).unwrap_or_default();
                    let value = f.expr().map(|e| self.build_expr(&e)).unwrap_or_else(|| {
                        // Shorthand: `Foo { x }` means `Foo { x: x }`.
                        if !fname.is_empty() {
                            let span = f
                                .name()
                                .map(|t| self.token_span(&t))
                                .unwrap_or_else(|| self.span(f.syntax()));
                            self.arena.alloc_expr(Expr {
                                span,
                                body: self.current_body(),
                                scope: self.current_scope(),
                                kind: ExprKind::NameRef(VirNameRefExpr {
                                    name: fname.clone(),
                                    name_span: span,
                                    resolved: None,
                                }),
                                ty: None,
                            })
                        } else {
                            self.error_expr(self.span(f.syntax()))
                        }
                    });
                    fields.push(StructLiteralField {
                        name: fname,
                        name_span: f
                            .name()
                            .map(|t| self.token_span(&t))
                            .unwrap_or_else(|| self.span(f.syntax())),
                        value,
                    });
                } else {
                    // Malformed child — emit error field.
                    let child_span = self.span(&child);
                    fields.push(StructLiteralField {
                        name: String::new(),
                        name_span: child_span,
                        value: self.error_expr(child_span),
                    });
                }
            }
        }

        self.arena.alloc_expr(Expr {
            span: sl_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::StructLiteral(VirStructLiteralExpr {
                name: name.clone(),
                name_span: sl.name().map(|t| self.token_span(&t)).unwrap_or(sl_span),
                resolved: None,
                fields,
            }),
            ty: None,
        })
    }

    fn build_field_expr(&mut self, f: &vx_parser::FieldExpr) -> ExprId {
        let field_span = self.span(f.syntax());
        let receiver = f
            .receiver()
            .map(|r| self.build_expr(&r))
            .unwrap_or_else(|| self.error_expr(field_span));
        // field_name() only returns ident tokens. For numeric tuple projections
        // like `.0`, we also check for an INT token after the dot.
        let field = f
            .field_name()
            .map(|t| {
                let text = t.text();
                if let Ok(idx) = text.parse::<u32>() {
                    FieldKey::Index(idx)
                } else if let Some(idx) = text
                    .strip_prefix('_')
                    .and_then(|digits| digits.parse::<u32>().ok())
                {
                    FieldKey::Index(idx)
                } else {
                    FieldKey::Name(text.to_string())
                }
            })
            .or_else(|| {
                // Look for an INT token (for tuple field access like `.0`).
                f.syntax()
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| t.kind() == SyntaxKind::INT)
                    .and_then(|t| t.text().parse::<u32>().ok())
                    .map(FieldKey::Index)
            })
            .unwrap_or_else(|| FieldKey::Name(String::new()));

        // field_span for the field key: use the ident token, or the INT token for
        // numeric projections, or fall back to the whole expression span.
        let key_span = f
            .field_name()
            .map(|t| self.token_span(&t))
            .or_else(|| {
                f.syntax()
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| t.kind() == SyntaxKind::INT)
                    .map(|t| self.token_span(&t))
            })
            .unwrap_or(field_span);

        self.arena.alloc_expr(Expr {
            span: field_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Field(VirFieldExpr {
                receiver,
                field,
                field_span: key_span,
            }),
            ty: None,
        })
    }

    fn build_index_expr(&mut self, idx: &vx_parser::IndexExpr) -> ExprId {
        let idx_span = self.span(idx.syntax());
        let receiver = idx
            .receiver()
            .map(|r| self.build_expr(&r))
            .unwrap_or_else(|| self.error_expr(idx_span));

        // A bare integer selector `recv[0]` is positional (tuple) projection.
        // Normalize it to a `FieldExpr { Index }` so it shares all the
        // static-resolution / demand / lowering machinery with `recv._0`.
        if let Some(index_ast) = idx.index()
            && let vx_parser::Expr::Literal(lit) = &index_ast
            && let Some(tok) = lit.token()
            && tok.kind() == SyntaxKind::INT
            && let Ok(n) = tok.text().parse::<u32>()
        {
            let key_span = self.span(index_ast.syntax());
            return self.arena.alloc_expr(Expr {
                span: idx_span,
                body: self.current_body(),
                scope: self.current_scope(),
                kind: ExprKind::Field(VirFieldExpr {
                    receiver,
                    field: FieldKey::Index(n),
                    field_span: key_span,
                }),
                ty: None,
            });
        }

        // General computed selection (filter, map, key lookup, …) — surface
        // exists, lowering is the next increment.
        let index = idx
            .index()
            .map(|i| self.build_expr(&i))
            .unwrap_or_else(|| self.error_expr(idx_span));

        self.arena.alloc_expr(Expr {
            span: idx_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Index(VirIndexExpr { receiver, index }),
            ty: None,
        })
    }

    fn build_bomb_expr(&mut self, b: &vx_parser::BombExpr) -> ExprId {
        let span = self.span(b.syntax());
        self.arena.alloc_expr(Expr {
            span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Bomb,
            ty: None,
        })
    }

    fn build_range_expr(&mut self, r: &vx_parser::RangeExpr) -> ExprId {
        let range_span = self.span(r.syntax());
        let start = r.start().map(|s| self.build_expr(&s));
        let end = r.end().map(|e| self.build_expr(&e));
        let inclusive = r.is_inclusive();

        self.arena.alloc_expr(Expr {
            span: range_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Range(VirRangeExpr {
                start,
                end,
                inclusive,
            }),
            ty: None,
        })
    }

    fn build_type_op_expr(&mut self, top: &vx_parser::TypeOpExpr) -> ExprId {
        let top_span = self.span(top.syntax());
        let expr = top
            .expr()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(top_span));
        let ty = top
            .ty()
            .map(|t| self.build_type_ref(&t))
            .unwrap_or_else(|| self.error_type_ref(top_span));

        self.arena.alloc_expr(Expr {
            span: top_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::TypeOp(VirTypeOpExpr {
                expr,
                op: {
                    top.operator_token()
                        .map(|t| match t.kind() {
                            SyntaxKind::AS_KW => expr::TypeOpKind::As,
                            SyntaxKind::IS_KW => expr::TypeOpKind::Is,
                            SyntaxKind::COLON2 => expr::TypeOpKind::Ascription,
                            _ => expr::TypeOpKind::As, // fallback
                        })
                        .unwrap_or(expr::TypeOpKind::As)
                },
                ty,
            }),
            ty: None,
        })
    }

    fn build_with_expr(&mut self, w: &vx_parser::WithExpr) -> ExprId {
        let with_span = self.span(w.syntax());
        let receiver = w
            .receiver()
            .map(|r| self.build_expr(&r))
            .unwrap_or_else(|| self.error_expr(with_span));

        // Parse UPDATE_BODY from raw syntax.
        let updates = w
            .syntax()
            .children()
            .find(|c| c.kind() == SyntaxKind::UPDATE_BODY)
            .map(|body| self.build_update_fields(&body))
            .unwrap_or_default();

        self.arena.alloc_expr(Expr {
            span: with_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::With(VirWithExpr { receiver, updates }),
            ty: None,
        })
    }

    fn build_update_fields(&mut self, body: &vx_parser::SyntaxNode) -> Vec<UpdateField> {
        body.children()
            .filter(|c| c.kind() == SyntaxKind::UPDATE_FIELD)
            .map(|field_node| {
                // Check if this is a nested update (has UPDATE_BODY child) or a set.
                let field_span = self.span(&field_node);
                if let Some(nested_body) = field_node
                    .children()
                    .find(|c| c.kind() == SyntaxKind::UPDATE_BODY)
                {
                    // Nested: `:field_path with { ... }`
                    let path = self.extract_field_path(&field_node);
                    UpdateField::Nested {
                        path,
                        path_span: field_span,
                        updates: self.build_update_fields(&nested_body),
                    }
                } else {
                    // Set: `field_path: expr`
                    let path = self.extract_field_path(&field_node);
                    let value = field_node
                        .children()
                        .find_map(vx_parser::Expr::cast)
                        .map(|e| self.build_expr(&e))
                        .unwrap_or_else(|| self.error_expr(field_span));
                    UpdateField::Set {
                        path,
                        path_span: field_span,
                        value,
                    }
                }
            })
            .collect()
    }

    fn extract_field_path(&self, node: &vx_parser::SyntaxNode) -> Vec<String> {
        // Field path is in a FIELD_PATH child or directly as LOWER_IDENT tokens.
        node.children()
            .find(|c| c.kind() == SyntaxKind::FIELD_PATH)
            .map(|fp| {
                fp.children_with_tokens()
                    .filter_map(|c| c.into_token())
                    .filter(|t| t.kind() == SyntaxKind::LOWER_IDENT)
                    .map(|t| t.text().to_string())
                    .collect()
            })
            .or_else(|| {
                let idents: Vec<String> = node
                    .children_with_tokens()
                    .filter_map(|c| c.into_token())
                    .filter(|t| t.kind() == SyntaxKind::LOWER_IDENT)
                    .map(|t| t.text().to_string())
                    .collect();
                if idents.is_empty() {
                    None
                } else {
                    Some(idents)
                }
            })
            .unwrap_or_default()
    }

    fn build_fallible_expr(&mut self, f: &vx_parser::FallibleExpr) -> ExprId {
        let f_span = self.span(f.syntax());
        let expr = f
            .expr()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(f_span));

        self.arena.alloc_expr(Expr {
            span: f_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Fallible(VirFallibleExpr { expr }),
            ty: None,
        })
    }

    fn build_fail_expr(&mut self, f: &vx_parser::FailExpr) -> ExprId {
        let f_span = self.span(f.syntax());
        let message = f.message().map(|m| self.build_expr(&m));

        self.arena.alloc_expr(Expr {
            span: f_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Fail(VirFailExpr { message }),
            ty: None,
        })
    }

    fn build_interpolated_string_expr(&mut self, is: &vx_parser::InterpolatedStringExpr) -> ExprId {
        let is_span = self.span(is.syntax());
        let parts: Vec<StringPart> = is
            .parts()
            .map(|part| match part {
                vx_parser::StringPart::Text(tok) => StringPart::Text(tok.text().to_string()),
                vx_parser::StringPart::Interpolation(interp) => {
                    if let Some(expr) = interp.expr() {
                        StringPart::Expr(self.build_expr(&expr))
                    } else {
                        StringPart::Expr(self.error_expr(self.span(interp.syntax())))
                    }
                }
            })
            .collect();

        self.arena.alloc_expr(Expr {
            span: is_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::InterpolatedString(VirInterpolatedStringExpr { parts }),
            ty: None,
        })
    }

    fn build_path_literal_expr(&mut self, pl: &vx_parser::PathLiteralExpr) -> ExprId {
        let pl_span = self.span(pl.syntax());

        // A path literal may contain interpolations.
        let parts: Vec<StringPart> = if let Some(interp) = pl.interpolated() {
            interp
                .parts()
                .map(|part| match part {
                    vx_parser::StringPart::Text(tok) => StringPart::Text(tok.text().to_string()),
                    vx_parser::StringPart::Interpolation(i) => {
                        if let Some(expr) = i.expr() {
                            StringPart::Expr(self.build_expr(&expr))
                        } else {
                            StringPart::Expr(self.error_expr(self.span(i.syntax())))
                        }
                    }
                })
                .collect()
        } else if let Some(lit) = pl.literal() {
            let text = lit
                .token()
                .map(|t| {
                    let s = t.text();
                    // Strip quotes if present.
                    if s.len() >= 2 && (s.starts_with('"') || s.starts_with('@')) {
                        // Path literals start with @, e.g. @"path"
                        let inner = s.trim_start_matches('@');
                        if inner.len() >= 2 {
                            inner[1..inner.len() - 1].to_string()
                        } else {
                            inner.to_string()
                        }
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_default();
            vec![StringPart::Text(text)]
        } else {
            vec![]
        };

        self.arena.alloc_expr(Expr {
            span: pl_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::PathLiteral(VirPathLiteralExpr { parts }),
            ty: None,
        })
    }

    fn build_import_expr(&mut self, i: &vx_parser::ImportExpr) -> ExprId {
        let import_span = self.span(i.syntax());
        let path = i.path().unwrap_or_default();

        self.arena.alloc_expr(Expr {
            span: import_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: ExprKind::Import(expr::ImportExpr {
                path,
                resolved_source: None,
            }),
            ty: None,
        })
    }

    // -----------------------------------------------------------------------
    // Statements
    // -----------------------------------------------------------------------

    /// Build a statement with a known declaration order. For let-statements,
    /// the decl_order is propagated to the binding registration so that
    /// bindings get the correct position for sequential visibility.
    fn build_stmt_with_order(&mut self, stmt: &vx_parser::Stmt, decl_order: u32) -> crate::StmtId {
        if let vx_parser::Stmt::Let(_) = stmt {
            self.current_let_decl_order = Some(decl_order);
            let result = self.build_stmt(stmt);
            self.current_let_decl_order = None;
            result
        } else {
            self.build_stmt(stmt)
        }
    }

    fn build_stmt(&mut self, stmt: &vx_parser::Stmt) -> crate::StmtId {
        match stmt {
            vx_parser::Stmt::Let(l) => self.build_let_stmt(l),
            vx_parser::Stmt::Assign(a) => self.build_assign_stmt(a),
            vx_parser::Stmt::CompoundUpdate(cu) => self.build_compound_update_stmt(cu),
            vx_parser::Stmt::FieldAssign(fa) => self.build_field_assign_stmt(fa),
            vx_parser::Stmt::Return(r) => self.build_return_stmt(r),
            vx_parser::Stmt::Expr(e) => {
                let expr_id = self.build_expr(e);
                self.arena.alloc_stmt(Stmt {
                    span: self.span(e.syntax()),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Expr(expr_id),
                })
            }
            // Item-as-statement cases: delegate to build_item.
            vx_parser::Stmt::FnDecl(f) => {
                let item_id = self.build_function_item(f);
                self.arena.alloc_stmt(Stmt {
                    span: self.span(f.syntax()),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Item(item_id),
                })
            }
            vx_parser::Stmt::StructDecl(s) => {
                let item_node = vx_parser::Item::StructDecl(s.clone());
                let item_id = self.build_item(&item_node);
                self.arena.alloc_stmt(Stmt {
                    span: self.span(s.syntax()),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Item(item_id),
                })
            }
            vx_parser::Stmt::EnumDecl(e) => {
                let item_node = vx_parser::Item::EnumDecl(e.clone());
                let item_id = self.build_item(&item_node);
                self.arena.alloc_stmt(Stmt {
                    span: self.span(e.syntax()),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Item(item_id),
                })
            }
            vx_parser::Stmt::NewtypeDecl(n) => {
                let item_node = vx_parser::Item::NewtypeDecl(n.clone());
                let item_id = self.build_item(&item_node);
                self.arena.alloc_stmt(Stmt {
                    span: self.span(n.syntax()),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Item(item_id),
                })
            }
            vx_parser::Stmt::TypeAliasDecl(ta) => {
                let item_node = vx_parser::Item::TypeAliasDecl(ta.clone());
                let item_id = self.build_item(&item_node);
                self.arena.alloc_stmt(Stmt {
                    span: self.span(ta.syntax()),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Item(item_id),
                })
            }
            vx_parser::Stmt::NamespaceDecl(ns) => {
                let item_node = vx_parser::Item::NamespaceDecl(ns.clone());
                let item_id = self.build_item(&item_node);
                self.arena.alloc_stmt(Stmt {
                    span: self.span(ns.syntax()),
                    body: self.current_body(),
                    scope: self.current_scope(),
                    kind: StmtKind::Item(item_id),
                })
            }
        }
    }

    fn build_let_stmt(&mut self, l: &vx_parser::LetStmt) -> crate::StmtId {
        let let_span = self.span(l.syntax());

        // Build init expr first (before pattern, so bindings aren't visible in init).
        let init = l
            .expr()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(let_span));

        let ty = l
            .slot_type()
            .and_then(|st| st.ty())
            .map(|t| self.build_type_ref(&t));

        let pattern = l
            .pattern()
            .map(|p| self.build_pattern(&p))
            .unwrap_or_else(|| self.error_pattern(let_span));

        // Back-patch init_expr for simple binding patterns.
        let binding_id = match self.arena[pattern].kind {
            PatternKind::Binding(ref b) => Some(b.binding),
            _ => None,
        };
        if let Some(bid) = binding_id {
            self.arena[bid].init_expr = Some(init);
        }

        self.arena.alloc_stmt(Stmt {
            span: let_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: StmtKind::Let(LetStmt { pattern, ty, init }),
        })
    }

    fn build_assign_stmt(&mut self, a: &vx_parser::AssignStmt) -> crate::StmtId {
        let assign_span = self.span(a.syntax());
        let name = a.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = a.name().map(|t| self.token_span(&t)).unwrap_or(assign_span);
        let value = a
            .expr()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(assign_span));

        self.arena.alloc_stmt(Stmt {
            span: assign_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: StmtKind::Assign(AssignStmt {
                name,
                name_span,
                value,
                resolved: None,
            }),
        })
    }

    fn build_compound_update_stmt(&mut self, cu: &vx_parser::CompoundUpdateStmt) -> crate::StmtId {
        let cu_span = self.span(cu.syntax());
        let name = cu.name().map(|t| t.text().to_string()).unwrap_or_default();
        let name_span = cu.name().map(|t| self.token_span(&t)).unwrap_or(cu_span);

        let updates = cu
            .syntax()
            .children()
            .find(|c| c.kind() == SyntaxKind::UPDATE_BODY)
            .map(|body| self.build_update_fields(&body))
            .unwrap_or_default();

        self.arena.alloc_stmt(Stmt {
            span: cu_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: StmtKind::CompoundUpdate(CompoundUpdateStmt {
                name,
                name_span,
                updates,
                resolved: None,
            }),
        })
    }

    fn build_field_assign_stmt(&mut self, fa: &vx_parser::FieldAssignStmt) -> crate::StmtId {
        let fa_span = self.span(fa.syntax());
        let value = fa
            .expr()
            .map(|e| self.build_expr(&e))
            .unwrap_or_else(|| self.error_expr(fa_span));

        let (base, field_path, target_span) = if let Some(target) = fa.target() {
            let segments: Vec<String> = target.segments().map(|t| t.text().to_string()).collect();
            let target_span = self.span(target.syntax());
            let base = segments.first().cloned().unwrap_or_default();
            let field_path = segments.into_iter().skip(1).collect();
            (base, field_path, target_span)
        } else {
            (String::new(), vec![], fa_span)
        };

        let base_span = fa
            .target()
            .and_then(|t| t.segments().next())
            .map(|t| self.token_span(&t))
            .unwrap_or(fa_span);

        self.arena.alloc_stmt(Stmt {
            span: fa_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: StmtKind::FieldAssign(FieldAssignStmt {
                base,
                base_span,
                field_path,
                target_span,
                value,
                resolved: None,
            }),
        })
    }

    fn build_return_stmt(&mut self, r: &vx_parser::ReturnStmt) -> crate::StmtId {
        let ret_span = self.span(r.syntax());
        let value = r.expr().map(|e| self.build_expr(&e));

        self.arena.alloc_stmt(Stmt {
            span: ret_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: StmtKind::Return(ReturnStmt { value }),
        })
    }

    // -----------------------------------------------------------------------
    // Patterns
    // -----------------------------------------------------------------------

    fn build_pattern(&mut self, pat: &vx_parser::Pattern) -> crate::PatternId {
        self.build_pattern_with_mutability(pat, false)
    }

    fn build_pattern_with_mutability(
        &mut self,
        pat: &vx_parser::Pattern,
        mutable: bool,
    ) -> crate::PatternId {
        match pat {
            vx_parser::Pattern::Paren(p) => {
                // Paren elimination: inner pattern's span.
                if let Some(inner) = p.inner() {
                    self.build_pattern_with_mutability(&inner, mutable)
                } else {
                    self.error_pattern(self.span(p.syntax()))
                }
            }
            vx_parser::Pattern::Binding(b) => self.build_binding_pattern(b, mutable),
            vx_parser::Pattern::Wildcard(w) => self.arena.alloc_pattern(Pattern {
                span: self.span(w.syntax()),
                body: self.current_body(),
                scope: self.current_scope(),
                kind: PatternKind::Wildcard,
                ty: None,
            }),
            vx_parser::Pattern::Literal(l) => self.build_literal_pattern(l),
            vx_parser::Pattern::Unit(u) => self.arena.alloc_pattern(Pattern {
                span: self.span(u.syntax()),
                body: self.current_body(),
                scope: self.current_scope(),
                kind: PatternKind::Unit,
                ty: None,
            }),
            vx_parser::Pattern::Tuple(t) => self.build_tuple_pattern(t),
            vx_parser::Pattern::Struct(s) => self.build_struct_pattern(s),
            vx_parser::Pattern::Array(a) => self.build_array_pattern(a),
            vx_parser::Pattern::ArrayRest(ar) => self.build_array_rest_pattern(ar),
            vx_parser::Pattern::Variant(v) => self.build_variant_pattern(v),
            vx_parser::Pattern::At(at) => self.build_at_pattern(at, mutable),
            vx_parser::Pattern::Or(or) => self.build_or_pattern(or),
        }
    }

    fn build_binding_pattern(
        &mut self,
        b: &vx_parser::BindingPattern,
        mutable: bool,
    ) -> crate::PatternId {
        let pat_span = self.span(b.syntax());
        let name = b.name().map(|t| t.text().to_string()).unwrap_or_default();

        if name == "_" {
            return self.arena.alloc_pattern(Pattern {
                span: pat_span,
                body: self.current_body(),
                scope: self.current_scope(),
                kind: PatternKind::Wildcard,
                ty: None,
            });
        }

        let name_span = b.name().map(|t| self.token_span(&t)).unwrap_or(pat_span);

        let binding_id = self.arena.alloc_binding(Binding {
            name: name.clone(),
            name_span,
            kind: BindingKind::Let,
            body: self.current_body(),
            scope: self.current_scope(),
            ty: None,
            init_expr: None,
            mutable,
        });
        let scope = self.current_scope();
        let order = self.arena[scope].entries.len() as u32;
        self.register_binding(scope, binding_id, order);

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Binding(PatternBinding {
                name,
                binding: binding_id,
            }),
            ty: None,
        })
    }

    fn build_literal_pattern(&mut self, l: &vx_parser::LiteralPattern) -> crate::PatternId {
        let pat_span = self.span(l.syntax());
        let kind = if let Some(token) = l.token() {
            match token.kind() {
                SyntaxKind::INT => LitKind::Int {
                    raw: token.text().to_string(),
                },
                SyntaxKind::FLOAT => LitKind::Float {
                    raw: token.text().to_string(),
                },
                SyntaxKind::STRING => {
                    let text = token.text();
                    let raw = if text.len() >= 2 {
                        text[1..text.len() - 1].to_string()
                    } else {
                        text.to_string()
                    };
                    LitKind::String { raw }
                }
                SyntaxKind::TRUE_KW => LitKind::Bool(true),
                SyntaxKind::FALSE_KW => LitKind::Bool(false),
                _ => LitKind::String {
                    raw: token.text().to_string(),
                },
            }
        } else {
            return self.error_pattern(pat_span);
        };

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Literal(VirPatternLiteral { value: kind }),
            ty: None,
        })
    }

    fn build_tuple_pattern(&mut self, t: &vx_parser::TuplePattern) -> crate::PatternId {
        let pat_span = self.span(t.syntax());
        let mut elements: Vec<crate::PatternId> = Vec::new();
        for child in t.syntax().children() {
            if let Some(p) = vx_parser::Pattern::cast(child.clone()) {
                elements.push(self.build_pattern(&p));
            } else {
                elements.push(self.error_pattern(self.span(&child)));
            }
        }

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Tuple(VirTuplePattern { elements }),
            ty: None,
        })
    }

    fn build_struct_pattern(&mut self, s: &vx_parser::StructPattern) -> crate::PatternId {
        let pat_span = self.span(s.syntax());
        let mut fields: Vec<VirStructPatternField> = Vec::new();
        for child in s.syntax().children() {
            if let Some(f) = vx_parser::StructPatternField::cast(child.clone()) {
                let name = f.name().map(|t| t.text().to_string()).unwrap_or_default();
                let pattern = f
                    .pattern()
                    .map(|p| self.build_pattern(&p))
                    .unwrap_or_else(|| {
                        // Shorthand: `{ x }` means `{ x: x }` — create a binding pattern.
                        if !name.is_empty() {
                            let span = f.name().map(|t| self.token_span(&t)).unwrap_or(pat_span);
                            let binding_id = self.arena.alloc_binding(Binding {
                                name: name.clone(),
                                name_span: span,
                                kind: BindingKind::PatternMatch,
                                body: self.current_body(),
                                scope: self.current_scope(),
                                ty: None,
                                init_expr: None,
                                mutable: false,
                            });
                            let scope = self.current_scope();
                            let order = self.arena[scope].entries.len() as u32;
                            self.register_binding(scope, binding_id, order);
                            self.arena.alloc_pattern(Pattern {
                                span,
                                body: self.current_body(),
                                scope: self.current_scope(),
                                kind: PatternKind::Binding(PatternBinding {
                                    name: name.clone(),
                                    binding: binding_id,
                                }),
                                ty: None,
                            })
                        } else {
                            self.error_pattern(self.span(f.syntax()))
                        }
                    });
                fields.push(VirStructPatternField {
                    name,
                    name_span: f
                        .name()
                        .map(|t| self.token_span(&t))
                        .unwrap_or_else(|| self.span(f.syntax())),
                    pattern,
                });
            } else {
                // Malformed child — emit error field.
                let child_span = self.span(&child);
                fields.push(VirStructPatternField {
                    name: String::new(),
                    name_span: child_span,
                    pattern: self.error_pattern(child_span),
                });
            }
        }

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Struct(VirStructPattern { fields }),
            ty: None,
        })
    }

    fn build_array_pattern(&mut self, a: &vx_parser::ArrayPattern) -> crate::PatternId {
        let pat_span = self.span(a.syntax());
        let mut elements: Vec<crate::PatternId> = Vec::new();
        for child in a.syntax().children() {
            if let Some(p) = vx_parser::Pattern::cast(child.clone()) {
                elements.push(self.build_pattern(&p));
            } else {
                elements.push(self.error_pattern(self.span(&child)));
            }
        }

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Array(VirArrayPattern { elements }),
            ty: None,
        })
    }

    fn build_array_rest_pattern(&mut self, ar: &vx_parser::ArrayRestPattern) -> crate::PatternId {
        let pat_span = self.span(ar.syntax());
        let binding = ar.name().map(|name_tok| {
            let name = name_tok.text().to_string();
            let name_span = self.token_span(&name_tok);
            let binding_id = self.arena.alloc_binding(Binding {
                name,
                name_span,
                kind: BindingKind::ArrayRest,
                body: self.current_body(),
                scope: self.current_scope(),
                ty: None,
                init_expr: None,
                mutable: false,
            });
            let scope = self.current_scope();
            let order = self.arena[scope].entries.len() as u32;
            self.register_binding(scope, binding_id, order);
            binding_id
        });

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::ArrayRest(VirArrayRestPattern {
                name: ar.name().map(|t| t.text().to_string()),
                binding,
            }),
            ty: None,
        })
    }

    fn build_variant_pattern(&mut self, v: &vx_parser::VariantPattern) -> crate::PatternId {
        let pat_span = self.span(v.syntax());
        let segments: Vec<String> = v.segments().map(|t| t.text().to_string()).collect();
        let name = segments.join(".");
        // Use the first segment's span as the name span, or fall back to pat_span.
        let name_span = v
            .segments()
            .next()
            .map(|t| self.token_span(&t))
            .unwrap_or(pat_span);

        let fields = v.fields().map(|sp| {
            let mut result = Vec::new();
            for child in sp.syntax().children() {
                if let Some(f) = vx_parser::StructPatternField::cast(child.clone()) {
                    let fname = f.name().map(|t| t.text().to_string()).unwrap_or_default();
                    let pattern =
                        f.pattern()
                            .map(|p| self.build_pattern(&p))
                            .unwrap_or_else(|| {
                                // Shorthand binding.
                                if !fname.is_empty() {
                                    let span =
                                        f.name().map(|t| self.token_span(&t)).unwrap_or(pat_span);
                                    let binding_id = self.arena.alloc_binding(Binding {
                                        name: fname.clone(),
                                        name_span: span,
                                        kind: BindingKind::PatternMatch,
                                        body: self.current_body(),
                                        scope: self.current_scope(),
                                        ty: None,
                                        init_expr: None,
                                        mutable: false,
                                    });
                                    let scope = self.current_scope();
                                    let order = self.arena[scope].entries.len() as u32;
                                    self.register_binding(scope, binding_id, order);
                                    self.arena.alloc_pattern(Pattern {
                                        span,
                                        body: self.current_body(),
                                        scope: self.current_scope(),
                                        kind: PatternKind::Binding(PatternBinding {
                                            name: fname.clone(),
                                            binding: binding_id,
                                        }),
                                        ty: None,
                                    })
                                } else {
                                    self.error_pattern(self.span(f.syntax()))
                                }
                            });
                    result.push(VirStructPatternField {
                        name: fname,
                        name_span: f.name().map(|t| self.token_span(&t)).unwrap_or(pat_span),
                        pattern,
                    });
                } else {
                    // Malformed child — emit error field.
                    let child_span = self.span(&child);
                    result.push(VirStructPatternField {
                        name: String::new(),
                        name_span: child_span,
                        pattern: self.error_pattern(child_span),
                    });
                }
            }
            result
        });

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Variant(VirVariantPattern {
                name,
                name_span,
                fields,
                resolved: None,
            }),
            ty: None,
        })
    }

    fn build_at_pattern(&mut self, at: &vx_parser::AtPattern, mutable: bool) -> crate::PatternId {
        let pat_span = self.span(at.syntax());
        let name = at
            .binding_name()
            .map(|t| t.text().to_string())
            .unwrap_or_default();
        let name_span = at
            .binding_name()
            .map(|t| self.token_span(&t))
            .unwrap_or(pat_span);

        let binding_id = self.arena.alloc_binding(Binding {
            name,
            name_span,
            kind: BindingKind::AtPattern,
            body: self.current_body(),
            scope: self.current_scope(),
            ty: None,
            init_expr: None,
            mutable,
        });
        let scope = self.current_scope();
        let order = self.arena[scope].entries.len() as u32;
        self.register_binding(scope, binding_id, order);

        let inner = at
            .pattern()
            .map(|p| self.build_pattern(&p))
            .unwrap_or_else(|| self.error_pattern(pat_span));

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::At(AtPattern {
                name: at
                    .binding_name()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default(),
                binding: binding_id,
                inner,
            }),
            ty: None,
        })
    }

    fn build_or_pattern(&mut self, or: &vx_parser::OrPattern) -> crate::PatternId {
        let pat_span = self.span(or.syntax());
        let alternatives: Vec<crate::PatternId> =
            or.patterns().map(|p| self.build_pattern(&p)).collect();

        self.arena.alloc_pattern(Pattern {
            span: pat_span,
            body: self.current_body(),
            scope: self.current_scope(),
            kind: PatternKind::Or(VirOrPattern { alternatives }),
            ty: None,
        })
    }

    // -----------------------------------------------------------------------
    // Type references
    // -----------------------------------------------------------------------

    fn build_type_ref(&mut self, ty: &vx_parser::Type) -> TypeRefId {
        match ty {
            vx_parser::Type::Paren(p) => {
                if let Some(inner) = p.inner() {
                    self.build_type_ref(&inner)
                } else {
                    self.error_type_ref(self.span(p.syntax()))
                }
            }
            vx_parser::Type::Path(p) => self.build_path_type_ref(p),
            vx_parser::Type::Function(f) => self.build_function_type_ref(f),
            vx_parser::Type::Tuple(t) => self.build_tuple_type_ref(t),
            vx_parser::Type::Optional(o) => self.build_optional_type_ref(o),
            vx_parser::Type::Sum(s) => self.build_sum_type_ref(s),
        }
    }

    fn build_path_type_ref(&mut self, p: &vx_parser::TypePath) -> TypeRefId {
        let type_span = self.span(p.syntax());
        let segments: Vec<String> = p.segments().map(|t| t.text().to_string()).collect();
        let type_args: Vec<TypeRefId> = p
            .type_args()
            .map(|ta| {
                let mut args = Vec::new();
                for child in ta.syntax().children() {
                    if let Some(t) = vx_parser::Type::cast(child.clone()) {
                        args.push(self.build_type_ref(&t));
                    } else {
                        args.push(self.error_type_ref(self.span(&child)));
                    }
                }
                args
            })
            .unwrap_or_default();

        self.arena.alloc_type_ref(TypeRef {
            span: type_span,
            kind: TypeRefKind::Path(PathTypeRef {
                segments,
                type_args,
            }),
        })
    }

    fn build_function_type_ref(&mut self, f: &vx_parser::FunctionType) -> TypeRefId {
        let type_span = self.span(f.syntax());
        // Walk raw children. All Type children except the last are parameters;
        // the last Type child is the return type. This is a recovery heuristic
        // matching the parser's FunctionType.parameter_types() convention, not a
        // structural guarantee — if the CST is broken, the split may be wrong.
        // Non-Type children that aren't CapabilityList or KeywordParameterList
        // get error type refs.
        let mut type_children: Vec<TypeRefId> = Vec::new();
        let mut capabilities: Vec<crate::body::CapabilityTag> = Vec::new();
        for child in f.syntax().children() {
            if let Some(t) = vx_parser::Type::cast(child.clone()) {
                type_children.push(self.build_type_ref(&t));
            } else if child.kind() == SyntaxKind::CAPABILITY_LIST {
                // Extract capability tags from the capability list's syntax children.
                for tag_node in child.children() {
                    if tag_node.kind() == SyntaxKind::CAPABILITY_TAG
                        && let Some(name_token) = tag_node
                            .children_with_tokens()
                            .filter_map(|c| c.into_token())
                            .find(|t| t.kind() == SyntaxKind::LOWER_IDENT)
                    {
                        capabilities.push(crate::body::CapabilityTag {
                            name: name_token.text().to_string(),
                            name_span: self.token_span(&name_token),
                        });
                    }
                }
            } else if child.kind() == SyntaxKind::KEYWORD_PARAMETER_LIST {
                // Expected non-Type structural child — skip.
            } else {
                type_children.push(self.error_type_ref(self.span(&child)));
            }
        }
        let ret = type_children
            .pop()
            .unwrap_or_else(|| self.error_type_ref(type_span));
        let params = type_children;

        self.arena.alloc_type_ref(TypeRef {
            span: type_span,
            kind: TypeRefKind::Function(FunctionTypeRef {
                params,
                ret,
                capabilities,
            }),
        })
    }

    fn build_tuple_type_ref(&mut self, t: &vx_parser::TupleType) -> TypeRefId {
        let type_span = self.span(t.syntax());
        let mut elements: Vec<TypeRefId> = Vec::new();
        for child in t.syntax().children() {
            if let Some(ty) = vx_parser::Type::cast(child.clone()) {
                elements.push(self.build_type_ref(&ty));
            } else {
                elements.push(self.error_type_ref(self.span(&child)));
            }
        }

        self.arena.alloc_type_ref(TypeRef {
            span: type_span,
            kind: TypeRefKind::Tuple(TupleTypeRef { elements }),
        })
    }

    fn build_optional_type_ref(&mut self, o: &vx_parser::OptionalType) -> TypeRefId {
        let type_span = self.span(o.syntax());
        let inner = o
            .inner()
            .map(|t| self.build_type_ref(&t))
            .unwrap_or_else(|| self.error_type_ref(type_span));

        self.arena.alloc_type_ref(TypeRef {
            span: type_span,
            kind: TypeRefKind::Optional(OptionalTypeRef { inner }),
        })
    }

    fn build_sum_type_ref(&mut self, s: &vx_parser::SumType) -> TypeRefId {
        let type_span = self.span(s.syntax());
        let mut alternatives: Vec<TypeRefId> = Vec::new();
        for child in s.syntax().children() {
            if let Some(ty) = vx_parser::Type::cast(child.clone()) {
                alternatives.push(self.build_type_ref(&ty));
            } else {
                alternatives.push(self.error_type_ref(self.span(&child)));
            }
        }

        self.arena.alloc_type_ref(TypeRef {
            span: type_span,
            kind: TypeRefKind::Sum(SumTypeRef { alternatives }),
        })
    }
}
