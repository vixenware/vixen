//! Scope tree construction from a Vixen CST.
//!
//! r[decl.scope]
//!
//! Walks the syntax tree and builds a tree of scopes, each containing
//! the definitions visible within it. Top-level items (types and functions)
//! are collected first (order-independent), then block-level bindings
//! are added sequentially.

use rowan::TextRange;
use vx_parser::{
    AstNode, BlockExpr, ClosureExpr, Expr, FnDecl, Item, LetStmt, MatchExpr, Pattern, SourceFile,
    Stmt, SyntaxKind, SyntaxNode,
};

/// Identifies a scope in the scope tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(u32);

impl ScopeId {
    pub const ROOT: ScopeId = ScopeId(0);

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// What kind of scope this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// Top-level module scope (source file).
    Module,
    /// Function body.
    Function,
    /// Block expression `{ ... }`.
    Block,
    /// Match arm (pattern bindings visible in the arm body).
    MatchArm,
    /// Closure `(params) => body`.
    Closure,
}

/// What kind of definition a name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    /// `fn name(...) { ... }`
    Function,
    /// `struct Name { ... }`
    Struct,
    /// `enum Name { ... }`
    Enum,
    /// `newtype Name of T`
    Newtype,
    /// `type Name = ...`
    TypeAlias,
    /// `let name = ...` or pattern binding
    Binding,
    /// Function parameter
    Parameter,
    /// Enum variant (accessed as `Enum.Variant`)
    Variant,
    /// Struct field
    Field,
    /// A builtin runtime function (not declared in source)
    Builtin,
}

/// A name definition in a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The name being defined.
    pub name: String,
    /// What kind of definition this is.
    pub kind: DefKind,
    /// Text range of the name token (for go-to-definition).
    pub name_range: TextRange,
    /// Text range of the entire declaration (for highlight).
    pub full_range: TextRange,
    /// The scope this definition belongs to.
    pub scope: ScopeId,
}

/// A single scope node in the tree.
#[derive(Debug)]
struct Scope {
    kind: ScopeKind,
    parent: Option<ScopeId>,
    text_range: TextRange,
    definitions: Vec<Definition>,
    children: Vec<ScopeId>,
}

/// The complete scope tree for a source file.
#[derive(Debug)]
pub struct ScopeTree {
    scopes: Vec<Scope>,
}

impl ScopeTree {
    /// Build a scope tree from a parsed source file.
    pub fn build(file: &SourceFile) -> Self {
        let mut builder = ScopeTreeBuilder { scopes: Vec::new() };
        builder.build_module(file);
        ScopeTree {
            scopes: builder.scopes,
        }
    }

    /// Find the innermost scope containing `offset`.
    pub fn scope_at(&self, offset: rowan::TextSize) -> ScopeId {
        let mut current = ScopeId::ROOT;
        'outer: loop {
            let scope = &self.scopes[current.index()];
            for &child_id in &scope.children {
                let child = &self.scopes[child_id.index()];
                if child.text_range.contains_inclusive(offset) {
                    current = child_id;
                    continue 'outer;
                }
            }
            return current;
        }
    }

    /// Look up a name from a given scope, walking up the scope chain.
    /// Returns the first matching definition.
    pub fn lookup(&self, name: &str, from: ScopeId) -> Option<&Definition> {
        let mut scope_id = Some(from);
        while let Some(id) = scope_id {
            let scope = &self.scopes[id.index()];
            // Search definitions in reverse order so later bindings shadow earlier ones
            if let Some(def) = scope.definitions.iter().rev().find(|d| d.name == name) {
                return Some(def);
            }
            scope_id = scope.parent;
        }
        None
    }

    /// Look up a name visible at a specific text offset within a scope.
    /// For sequential scopes (blocks), only definitions before the offset are visible.
    pub fn lookup_at(
        &self,
        name: &str,
        from: ScopeId,
        offset: rowan::TextSize,
    ) -> Option<&Definition> {
        let mut scope_id = Some(from);
        while let Some(id) = scope_id {
            let scope = &self.scopes[id.index()];
            match scope.kind {
                // In module scope, all items are visible regardless of position
                ScopeKind::Module => {
                    if let Some(def) = scope.definitions.iter().rev().find(|d| d.name == name) {
                        return Some(def);
                    }
                }
                // In blocks, only bindings defined before the reference are visible
                ScopeKind::Block => {
                    if let Some(def) = scope
                        .definitions
                        .iter()
                        .rev()
                        .find(|d| d.name == name && d.name_range.start() < offset)
                    {
                        return Some(def);
                    }
                }
                // In function/closure/match-arm scopes, all params are visible
                _ => {
                    if let Some(def) = scope.definitions.iter().rev().find(|d| d.name == name) {
                        return Some(def);
                    }
                }
            }
            scope_id = scope.parent;
        }
        None
    }

    /// Iterate all definitions in a scope (not including parent scopes).
    pub fn definitions(&self, scope: ScopeId) -> &[Definition] {
        &self.scopes[scope.index()].definitions
    }

    /// Get the kind of a scope.
    pub fn scope_kind(&self, scope: ScopeId) -> ScopeKind {
        self.scopes[scope.index()].kind
    }

    /// Get the parent scope.
    pub fn parent(&self, scope: ScopeId) -> Option<ScopeId> {
        self.scopes[scope.index()].parent
    }

    /// Get the text range of a scope.
    pub fn text_range(&self, scope: ScopeId) -> TextRange {
        self.scopes[scope.index()].text_range
    }

    /// Iterate all definitions across all scopes.
    pub fn all_definitions(&self) -> impl Iterator<Item = &Definition> {
        self.scopes.iter().flat_map(|s| s.definitions.iter())
    }

    /// Enumerate all definitions visible at a given offset, walking up the scope chain.
    /// Respects sequential visibility in blocks (only defs before offset).
    /// Later bindings shadow earlier ones with the same name.
    pub fn visible_at(&self, offset: rowan::TextSize) -> Vec<&Definition> {
        let from = self.scope_at(offset);
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        let mut scope_id = Some(from);

        while let Some(id) = scope_id {
            let scope = &self.scopes[id.index()];
            for def in scope.definitions.iter().rev() {
                // In blocks, only show defs before the cursor
                if scope.kind == ScopeKind::Block && def.name_range.start() >= offset {
                    continue;
                }
                // First occurrence wins (shadowing)
                if seen.insert(&def.name) {
                    result.push(def);
                }
            }
            scope_id = scope.parent;
        }

        result
    }

    /// Get the number of scopes.
    pub fn scope_count(&self) -> usize {
        self.scopes.len()
    }
}

struct ScopeTreeBuilder {
    scopes: Vec<Scope>,
}

impl ScopeTreeBuilder {
    fn push_scope(
        &mut self,
        kind: ScopeKind,
        parent: Option<ScopeId>,
        text_range: TextRange,
    ) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope {
            kind,
            parent,
            text_range,
            definitions: Vec::new(),
            children: Vec::new(),
        });
        if let Some(parent_id) = parent {
            self.scopes[parent_id.index()].children.push(id);
        }
        id
    }

    fn add_def(&mut self, scope: ScopeId, def: Definition) {
        self.scopes[scope.index()].definitions.push(def);
    }

    fn build_module(&mut self, file: &SourceFile) {
        let module_scope = self.push_scope(ScopeKind::Module, None, file.syntax().text_range());

        // r[decl.scope] — collect all top-level items (order-independent)
        for item in file.items() {
            self.collect_item_def(module_scope, &item);
        }

        // Now descend into function bodies to build inner scopes
        for item in file.items() {
            if let Item::FnDecl(fn_decl) = item {
                self.build_fn_scope(module_scope, &fn_decl);
            }
        }
    }

    fn collect_item_def(&mut self, scope: ScopeId, item: &Item) {
        let (name, kind, name_range, full_range) = match item {
            Item::FnDecl(decl) => {
                let Some(name_tok) = decl.name() else {
                    return;
                };
                (
                    name_tok.text().to_string(),
                    DefKind::Function,
                    name_tok.text_range(),
                    decl.syntax().text_range(),
                )
            }
            Item::StructDecl(decl) => {
                let Some(name_tok) = decl.name() else {
                    return;
                };
                (
                    name_tok.text().to_string(),
                    DefKind::Struct,
                    name_tok.text_range(),
                    decl.syntax().text_range(),
                )
            }
            Item::EnumDecl(decl) => {
                let Some(name_tok) = decl.name() else {
                    return;
                };
                (
                    name_tok.text().to_string(),
                    DefKind::Enum,
                    name_tok.text_range(),
                    decl.syntax().text_range(),
                )
            }
            Item::NewtypeDecl(decl) => {
                let Some(name_tok) = decl.name() else {
                    return;
                };
                (
                    name_tok.text().to_string(),
                    DefKind::Newtype,
                    name_tok.text_range(),
                    decl.syntax().text_range(),
                )
            }
            Item::TypeAliasDecl(decl) => {
                let Some(name_tok) = decl.name() else {
                    return;
                };
                (
                    name_tok.text().to_string(),
                    DefKind::TypeAlias,
                    name_tok.text_range(),
                    decl.syntax().text_range(),
                )
            }
        };
        self.add_def(
            scope,
            Definition {
                name,
                kind,
                name_range,
                full_range,
                scope,
            },
        );
    }

    // r[decl.fn]
    fn build_fn_scope(&mut self, parent: ScopeId, fn_decl: &FnDecl) {
        let fn_scope = self.push_scope(
            ScopeKind::Function,
            Some(parent),
            fn_decl.syntax().text_range(),
        );

        // r[decl.fn.destructure] — parameters introduce bindings
        for param in fn_decl.parameters() {
            if let Some(name_tok) = param.name() {
                self.add_def(
                    fn_scope,
                    Definition {
                        name: name_tok.text().to_string(),
                        kind: DefKind::Parameter,
                        name_range: name_tok.text_range(),
                        full_range: param.syntax().text_range(),
                        scope: fn_scope,
                    },
                );
            }
        }

        // r[decl.fn.keyword-args] — keyword parameters
        for kw_param in fn_decl.keyword_parameters() {
            if let Some(name_tok) = kw_param.name() {
                self.add_def(
                    fn_scope,
                    Definition {
                        name: name_tok.text().to_string(),
                        kind: DefKind::Parameter,
                        name_range: name_tok.text_range(),
                        full_range: kw_param.syntax().text_range(),
                        scope: fn_scope,
                    },
                );
            }
        }

        // Descend into body
        if let Some(body) = fn_decl.body() {
            self.build_block_scope(fn_scope, &body);
        }
    }

    // r[control.block]
    fn build_block_scope(&mut self, parent: ScopeId, block: &BlockExpr) {
        let block_scope =
            self.push_scope(ScopeKind::Block, Some(parent), block.syntax().text_range());

        for stmt in block.statements() {
            self.visit_stmt(block_scope, &stmt);
        }
    }

    fn visit_stmt(&mut self, scope: ScopeId, stmt: &Stmt) {
        match stmt {
            // r[decl.binding]
            Stmt::Let(let_stmt) => {
                // The initializer expression is visited first (it can't reference
                // the binding being defined)
                if let Some(init) = let_stmt.expr() {
                    self.visit_expr(scope, &init);
                }
                // r[decl.binding.destructure] — collect bindings from the pattern
                self.collect_let_bindings(scope, let_stmt);
            }
            Stmt::Assign(assign) => {
                if let Some(expr) = assign.expr() {
                    self.visit_expr(scope, &expr);
                }
            }
            Stmt::Return(ret) => {
                if let Some(expr) = ret.expr() {
                    self.visit_expr(scope, &expr);
                }
            }
            Stmt::FnDecl(fn_decl) => {
                // Local function — add to current scope
                if let Some(name_tok) = fn_decl.name() {
                    self.add_def(
                        scope,
                        Definition {
                            name: name_tok.text().to_string(),
                            kind: DefKind::Function,
                            name_range: name_tok.text_range(),
                            full_range: fn_decl.syntax().text_range(),
                            scope,
                        },
                    );
                }
                self.build_fn_scope(scope, fn_decl);
            }
            Stmt::Expr(expr) => {
                self.visit_expr(scope, expr);
            }
            Stmt::CompoundUpdate(_) | Stmt::FieldAssign(_) => {
                // These reference existing bindings; resolution handles them
            }
        }
    }

    fn visit_expr(&mut self, scope: ScopeId, expr: &Expr) {
        match expr {
            // r[expr.block]
            Expr::Block(block) => {
                self.build_block_scope(scope, block);
            }
            // r[expr.closure]
            Expr::Closure(closure) => {
                self.build_closure_scope(scope, closure);
            }
            // r[expr.if]
            Expr::If(if_expr) => {
                // Visit condition and then descend into blocks
                self.visit_expr_children(scope, if_expr.syntax());
            }
            // r[expr.match]
            Expr::Match(match_expr) => {
                self.build_match_scopes(scope, match_expr);
            }
            // For all other expressions, just recurse into child expressions
            _ => {
                self.visit_expr_children(scope, expr.syntax());
            }
        }
    }

    fn visit_expr_children(&mut self, scope: ScopeId, node: &SyntaxNode) {
        for child in node.children() {
            if let Some(expr) = Expr::cast(child.clone()) {
                self.visit_expr(scope, &expr);
            } else if let Some(block) = BlockExpr::cast(child.clone()) {
                self.build_block_scope(scope, &block);
            } else if matches!(
                child.kind(),
                SyntaxKind::ELSE_CLAUSE
                    | SyntaxKind::MATCH_ARM
                    | SyntaxKind::ARG_LIST
                    | SyntaxKind::ARG
                    | SyntaxKind::KEYWORD_ARG
            ) {
                // Recurse into structural children
                self.visit_expr_children(scope, &child);
            }
        }
    }

    fn build_closure_scope(&mut self, parent: ScopeId, closure: &ClosureExpr) {
        let closure_scope = self.push_scope(
            ScopeKind::Closure,
            Some(parent),
            closure.syntax().text_range(),
        );

        // Closure parameters
        for param in closure.parameters() {
            if let Some(name_tok) = param.name() {
                self.add_def(
                    closure_scope,
                    Definition {
                        name: name_tok.text().to_string(),
                        kind: DefKind::Parameter,
                        name_range: name_tok.text_range(),
                        full_range: param.syntax().text_range(),
                        scope: closure_scope,
                    },
                );
            }
        }

        // Descend into body
        if let Some(body) = closure.body() {
            self.visit_expr(closure_scope, &body);
        }
    }

    // r[control.match]
    fn build_match_scopes(&mut self, parent: ScopeId, match_expr: &MatchExpr) {
        // Visit the scrutinee in the parent scope
        if let Some(scrutinee) = match_expr.syntax().children().find_map(Expr::cast) {
            self.visit_expr(parent, &scrutinee);
        }

        // Each match arm gets its own scope for pattern bindings
        for arm in match_expr.arms() {
            let arm_scope =
                self.push_scope(ScopeKind::MatchArm, Some(parent), arm.syntax().text_range());

            // r[pattern.binding] — collect bindings from the arm's pattern
            if let Some(pat) = arm.pattern() {
                self.collect_pattern_bindings(arm_scope, &pat);
            }

            // Visit the arm body expression
            if let Some(body) = arm.expr() {
                self.visit_expr(arm_scope, &body);
            }
        }
    }

    fn collect_let_bindings(&mut self, scope: ScopeId, let_stmt: &LetStmt) {
        // The let statement itself may use simple name or destructuring pattern.
        // Walk the pattern subtree to find all binding names.
        let syntax = let_stmt.syntax();

        // Find the pattern node(s) in the let statement
        for child in syntax.children() {
            if let Some(pat) = Pattern::cast(child) {
                self.collect_pattern_bindings(scope, &pat);
                return;
            }
        }

        // Fallback: simple `let name = expr` without explicit pattern wrapper
        if let Some(name_tok) = let_stmt.name()
            && name_tok.kind() != SyntaxKind::UNDERSCORE
        {
            self.add_def(
                scope,
                Definition {
                    name: name_tok.text().to_string(),
                    kind: DefKind::Binding,
                    name_range: name_tok.text_range(),
                    full_range: syntax.text_range(),
                    scope,
                },
            );
        }
    }

    fn collect_pattern_bindings(&mut self, scope: ScopeId, pattern: &Pattern) {
        match pattern {
            Pattern::Binding(bp) => {
                if let Some(name_tok) = bp.name()
                    && name_tok.kind() != SyntaxKind::UNDERSCORE
                {
                    self.add_def(
                        scope,
                        Definition {
                            name: name_tok.text().to_string(),
                            kind: DefKind::Binding,
                            name_range: name_tok.text_range(),
                            full_range: bp.syntax().text_range(),
                            scope,
                        },
                    );
                }
            }
            Pattern::Tuple(tp) => {
                for sub in tp.items() {
                    self.collect_pattern_bindings(scope, &sub);
                }
            }
            Pattern::Struct(sp) => {
                for field in sp.fields() {
                    if let Some(sub_pat) = field.pattern() {
                        self.collect_pattern_bindings(scope, &sub_pat);
                    } else if let Some(name_tok) = field.name() {
                        // Shorthand `{ field }` binds `field`
                        self.add_def(
                            scope,
                            Definition {
                                name: name_tok.text().to_string(),
                                kind: DefKind::Binding,
                                name_range: name_tok.text_range(),
                                full_range: field.syntax().text_range(),
                                scope,
                            },
                        );
                    }
                }
            }
            Pattern::Array(ap) => {
                for sub in ap.items() {
                    self.collect_pattern_bindings(scope, &sub);
                }
            }
            Pattern::ArrayRest(rp) => {
                if let Some(name_tok) = rp.name() {
                    self.add_def(
                        scope,
                        Definition {
                            name: name_tok.text().to_string(),
                            kind: DefKind::Binding,
                            name_range: name_tok.text_range(),
                            full_range: rp.syntax().text_range(),
                            scope,
                        },
                    );
                }
            }
            Pattern::Variant(vp) => {
                if let Some(sp) = vp.fields() {
                    for field in sp.fields() {
                        if let Some(sub_pat) = field.pattern() {
                            self.collect_pattern_bindings(scope, &sub_pat);
                        } else if let Some(name_tok) = field.name() {
                            self.add_def(
                                scope,
                                Definition {
                                    name: name_tok.text().to_string(),
                                    kind: DefKind::Binding,
                                    name_range: name_tok.text_range(),
                                    full_range: field.syntax().text_range(),
                                    scope,
                                },
                            );
                        }
                    }
                }
            }
            Pattern::At(at) => {
                if let Some(name_tok) = at.binding_name() {
                    self.add_def(
                        scope,
                        Definition {
                            name: name_tok.text().to_string(),
                            kind: DefKind::Binding,
                            name_range: name_tok.text_range(),
                            full_range: at.syntax().text_range(),
                            scope,
                        },
                    );
                }
                if let Some(sub) = at.pattern() {
                    self.collect_pattern_bindings(scope, &sub);
                }
            }
            Pattern::Or(op) => {
                // r[pattern.or] — all alternatives must bind the same names
                // We collect from the first alternative only (semantic check later)
                if let Some(first) = op.patterns().next() {
                    self.collect_pattern_bindings(scope, &first);
                }
            }
            Pattern::Paren(pp) => {
                if let Some(inner) = pp.inner() {
                    self.collect_pattern_bindings(scope, &inner);
                }
            }
            // These don't introduce bindings
            Pattern::Wildcard(_) | Pattern::Literal(_) | Pattern::Unit(_) => {}
        }
    }
}
