//! Scope tree construction from a Vixen CST.
//!
//! r[decl.scope]
//!
//! Walks the syntax tree and builds a tree of scopes, each containing
//! the definitions visible within it. Top-level items (types and functions)
//! are collected first (order-independent), then block-level bindings
//! are added sequentially.

use std::collections::{HashMap, HashSet};

use rowan::TextRange;
use vx_parser::{
    AstNode, BlockExpr, ClosureExpr, EnumDecl, Expr, FnDecl, Item, LetStmt, MatchExpr, Pattern,
    SourceFile, Stmt, SyntaxKind, SyntaxNode,
};

use crate::{ImportGraph, SourceFileRef, SourceId, SourceSpan};

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
    /// Namespace body.
    Namespace,
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
    /// `namespace Name { ... }`
    Namespace,
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
}

pub type DefSite = SourceSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefId {
    Source { source: SourceId, local: u32 },
}

/// A name definition in a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// Stable identity for this definition.
    pub id: DefId,
    /// The name being defined.
    pub name: String,
    /// What kind of definition this is.
    pub kind: DefKind,
    /// Text range of the name token (for go-to-definition).
    pub name_span: SourceSpan,
    /// Text range of the entire declaration (for highlight).
    pub full_span: SourceSpan,
    /// The scope this definition belongs to.
    pub scope: ScopeId,
}

impl Definition {
    pub fn source(&self) -> SourceId {
        self.name_span.source
    }

    pub fn site(&self) -> DefSite {
        self.name_span
    }
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
    owned_scopes: HashMap<DefSite, ScopeId>,
}

#[derive(Debug)]
pub struct ProgramScopes {
    trees: HashMap<SourceId, ScopeTree>,
    imports: ImportGraph,
}

impl ScopeTree {
    /// Build a scope tree from a parsed source file.
    pub fn build(file: &SourceFile) -> Self {
        Self::build_source(SourceFileRef {
            id: SourceId(0),
            file: file.clone(),
        })
    }

    pub fn build_source(source: SourceFileRef) -> Self {
        let mut builder = ScopeTreeBuilder {
            scopes: Vec::new(),
            source: source.id,
            next_def_id: 0,
            owned_scopes: HashMap::new(),
        };
        builder.build_module(&source.file);
        ScopeTree {
            scopes: builder.scopes,
            owned_scopes: builder.owned_scopes,
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
                ScopeKind::Module | ScopeKind::Namespace => {
                    if let Some(def) = scope.definitions.iter().rev().find(|d| d.name == name) {
                        if def.kind != DefKind::Binding || def.name_span.range.start() < offset {
                            return Some(def);
                        }
                    }
                }
                // In blocks, only bindings defined before the reference are visible
                ScopeKind::Block => {
                    if let Some(def) = scope
                        .definitions
                        .iter()
                        .rev()
                        .find(|d| d.name == name && d.name_span.range.start() < offset)
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
                if scope.kind == ScopeKind::Block && def.name_span.range.start() >= offset {
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

    pub fn owned_scope(&self, def: &Definition) -> Option<ScopeId> {
        self.owned_scopes.get(&def.site()).copied()
    }
}

impl ProgramScopes {
    pub fn build(sources: &[SourceFileRef], imports: ImportGraph) -> Self {
        let trees = sources
            .iter()
            .map(|source| (source.id, ScopeTree::build_source(source.clone())))
            .collect();
        Self { trees, imports }
    }

    pub fn tree(&self, source: SourceId) -> Option<&ScopeTree> {
        self.trees.get(&source)
    }

    pub fn scope_at(&self, source: SourceId, offset: rowan::TextSize) -> Option<ScopeId> {
        self.tree(source).map(|tree| tree.scope_at(offset))
    }

    pub fn scope_kind(&self, source: SourceId, scope: ScopeId) -> Option<ScopeKind> {
        self.tree(source).map(|tree| tree.scope_kind(scope))
    }

    pub fn text_range(&self, source: SourceId, scope: ScopeId) -> Option<TextRange> {
        self.tree(source).map(|tree| tree.text_range(scope))
    }

    pub fn definitions(&self, source: SourceId, scope: ScopeId) -> Option<&[Definition]> {
        self.tree(source).map(|tree| tree.definitions(scope))
    }

    pub fn all_definitions_in(&self, source: SourceId) -> Vec<&Definition> {
        self.tree(source)
            .into_iter()
            .flat_map(ScopeTree::all_definitions)
            .collect()
    }

    pub fn lookup_at(
        &self,
        source: SourceId,
        name: &str,
        from: ScopeId,
        offset: rowan::TextSize,
    ) -> Option<&Definition> {
        if let Some(tree) = self.tree(source)
            && let Some(def) = tree.lookup_at(name, from, offset)
        {
            return Some(def);
        }

        let mut visited = HashSet::new();
        self.lookup_imported(source, name, &mut visited)
    }

    pub fn visible_at(&self, source: SourceId, offset: rowan::TextSize) -> Vec<&Definition> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();

        if let Some(tree) = self.tree(source) {
            for def in tree.visible_at(offset) {
                if seen.insert(def.name.as_str()) {
                    result.push(def);
                }
            }
        }

        let mut visited = HashSet::new();
        self.collect_imported_visible(source, &mut seen, &mut visited, &mut result);
        result
    }

    pub fn owned_scope(&self, def: &Definition) -> Option<ScopeId> {
        self.tree(def.source())?.owned_scope(def)
    }

    pub fn lookup_member(&self, namespace: &Definition, name: &str) -> Option<&Definition> {
        let scope = self.owned_scope(namespace)?;
        self.definitions(namespace.source(), scope)?
            .iter()
            .find(|def| def.name == name)
    }

    pub fn lookup_members<'a>(&'a self, namespace: &Definition, name: &str) -> Vec<&'a Definition> {
        let Some(scope) = self.owned_scope(namespace) else {
            return Vec::new();
        };
        let Some(definitions) = self.definitions(namespace.source(), scope) else {
            return Vec::new();
        };
        definitions.iter().filter(|def| def.name == name).collect()
    }

    fn lookup_imported<'a>(
        &'a self,
        source: SourceId,
        name: &str,
        visited: &mut HashSet<SourceId>,
    ) -> Option<&'a Definition> {
        if !visited.insert(source) {
            return None;
        }

        for &imported in self.imports.imports_from(source).iter().rev() {
            if let Some(tree) = self.tree(imported)
                && let Some(def) = tree
                    .definitions(ScopeId::ROOT)
                    .iter()
                    .rev()
                    .find(|def| def.name == name)
            {
                return Some(def);
            }

            if let Some(def) = self.lookup_imported(imported, name, visited) {
                return Some(def);
            }
        }

        None
    }

    fn collect_imported_visible<'a>(
        &'a self,
        source: SourceId,
        seen: &mut HashSet<&'a str>,
        visited: &mut HashSet<SourceId>,
        result: &mut Vec<&'a Definition>,
    ) {
        if !visited.insert(source) {
            return;
        }

        for &imported in self.imports.imports_from(source).iter().rev() {
            if let Some(tree) = self.tree(imported) {
                for def in tree.definitions(ScopeId::ROOT).iter().rev() {
                    if seen.insert(def.name.as_str()) {
                        result.push(def);
                    }
                }
            }

            self.collect_imported_visible(imported, seen, visited, result);
        }
    }
}

struct ScopeTreeBuilder {
    scopes: Vec<Scope>,
    source: SourceId,
    next_def_id: u32,
    owned_scopes: HashMap<DefSite, ScopeId>,
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

        for item in file.items() {
            match item {
                Item::FnDecl(fn_decl) => self.build_fn_scope(module_scope, &fn_decl),
                Item::NamespaceDecl(namespace_decl) => {
                    self.build_namespace_scope(module_scope, &namespace_decl);
                }
                Item::StructDecl(_)
                | Item::EnumDecl(_)
                | Item::NewtypeDecl(_)
                | Item::TypeAliasDecl(_) => {}
            }
        }

        for stmt in file.statements() {
            match stmt {
                Stmt::FnDecl(fn_decl) => self.build_fn_scope(module_scope, &fn_decl),
                Stmt::StructDecl(_)
                | Stmt::EnumDecl(_)
                | Stmt::NewtypeDecl(_)
                | Stmt::TypeAliasDecl(_) => {}
                _ => self.visit_stmt(module_scope, &stmt),
            }
        }
    }

    fn build_namespace_scope(
        &mut self,
        parent: ScopeId,
        namespace_decl: &vx_parser::NamespaceDecl,
    ) {
        let namespace_scope = self.push_scope(
            ScopeKind::Namespace,
            Some(parent),
            namespace_decl.syntax().text_range(),
        );
        if let Some(name_tok) = namespace_decl.name() {
            self.owned_scopes.insert(
                SourceSpan::new(self.source, name_tok.text_range()),
                namespace_scope,
            );
        }

        let Some(body) = namespace_decl.body() else {
            return;
        };

        for item in body.items() {
            self.collect_item_def(namespace_scope, &item);
        }

        for item in body.items() {
            match item {
                Item::FnDecl(fn_decl) => self.build_fn_scope(namespace_scope, &fn_decl),
                Item::NamespaceDecl(namespace_decl) => {
                    self.build_namespace_scope(namespace_scope, &namespace_decl);
                }
                Item::StructDecl(_)
                | Item::EnumDecl(_)
                | Item::NewtypeDecl(_)
                | Item::TypeAliasDecl(_) => {}
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
            Item::NamespaceDecl(decl) => {
                let Some(name_tok) = decl.name() else {
                    return;
                };
                (
                    name_tok.text().to_string(),
                    DefKind::Namespace,
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
                if name_tok.text() == "Option" {
                    self.collect_enum_variant_defs(scope, decl);
                }
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
        self.add_named_def(scope, name, kind, name_range, full_range);
    }

    fn add_named_def(
        &mut self,
        scope: ScopeId,
        name: String,
        kind: DefKind,
        name_range: TextRange,
        full_range: TextRange,
    ) {
        let def = self.named_definition(scope, name, kind, name_range, full_range);
        self.add_def(scope, def);
    }

    fn collect_enum_variant_defs(&mut self, scope: ScopeId, decl: &EnumDecl) {
        for variant in decl.variants() {
            let Some(name_tok) = variant.name() else {
                continue;
            };
            self.add_named_def(
                scope,
                name_tok.text().to_string(),
                DefKind::Variant,
                name_tok.text_range(),
                variant.syntax().text_range(),
            );
        }
    }

    fn alloc_def_id(&mut self) -> u32 {
        let id = self.next_def_id;
        self.next_def_id += 1;
        id
    }

    fn named_definition(
        &mut self,
        scope: ScopeId,
        name: String,
        kind: DefKind,
        name_range: TextRange,
        full_range: TextRange,
    ) -> Definition {
        Definition {
            id: DefId::Source {
                source: self.source,
                local: self.alloc_def_id(),
            },
            name,
            kind,
            name_span: SourceSpan::new(self.source, name_range),
            full_span: SourceSpan::new(self.source, full_range),
            scope,
        }
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
                let def = self.named_definition(
                    fn_scope,
                    name_tok.text().to_string(),
                    DefKind::Parameter,
                    name_tok.text_range(),
                    param.syntax().text_range(),
                );
                self.add_def(fn_scope, def);
            }
        }

        // r[decl.fn.keyword-args] — keyword parameters
        for kw_param in fn_decl.keyword_parameters() {
            if let Some(name_tok) = kw_param.name() {
                let def = self.named_definition(
                    fn_scope,
                    name_tok.text().to_string(),
                    DefKind::Parameter,
                    name_tok.text_range(),
                    kw_param.syntax().text_range(),
                );
                self.add_def(fn_scope, def);
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
                    self.add_named_def(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Function,
                        name_tok.text_range(),
                        fn_decl.syntax().text_range(),
                    );
                }
                self.build_fn_scope(scope, fn_decl);
            }
            Stmt::StructDecl(decl) => {
                if let Some(name_tok) = decl.name() {
                    self.add_named_def(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Struct,
                        name_tok.text_range(),
                        decl.syntax().text_range(),
                    );
                }
            }
            Stmt::EnumDecl(decl) => {
                if let Some(name_tok) = decl.name() {
                    self.add_named_def(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Enum,
                        name_tok.text_range(),
                        decl.syntax().text_range(),
                    );
                }
                self.collect_enum_variant_defs(scope, decl);
            }
            Stmt::NewtypeDecl(decl) => {
                if let Some(name_tok) = decl.name() {
                    self.add_named_def(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Newtype,
                        name_tok.text_range(),
                        decl.syntax().text_range(),
                    );
                }
            }
            Stmt::TypeAliasDecl(decl) => {
                if let Some(name_tok) = decl.name() {
                    self.add_named_def(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::TypeAlias,
                        name_tok.text_range(),
                        decl.syntax().text_range(),
                    );
                }
            }
            Stmt::NamespaceDecl(decl) => {
                if let Some(name_tok) = decl.name() {
                    self.add_named_def(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Namespace,
                        name_tok.text_range(),
                        decl.syntax().text_range(),
                    );
                }
                self.build_namespace_scope(scope, decl);
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
            } else {
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
                let def = self.named_definition(
                    closure_scope,
                    name_tok.text().to_string(),
                    DefKind::Parameter,
                    name_tok.text_range(),
                    param.syntax().text_range(),
                );
                self.add_def(closure_scope, def);
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
            let def = self.named_definition(
                scope,
                name_tok.text().to_string(),
                DefKind::Binding,
                name_tok.text_range(),
                syntax.text_range(),
            );
            self.add_def(scope, def);
        }
    }

    fn collect_pattern_bindings(&mut self, scope: ScopeId, pattern: &Pattern) {
        match pattern {
            Pattern::Binding(bp) => {
                if let Some(name_tok) = bp.name()
                    && name_tok.kind() != SyntaxKind::UNDERSCORE
                {
                    let def = self.named_definition(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Binding,
                        name_tok.text_range(),
                        bp.syntax().text_range(),
                    );
                    self.add_def(scope, def);
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
                        let def = self.named_definition(
                            scope,
                            name_tok.text().to_string(),
                            DefKind::Binding,
                            name_tok.text_range(),
                            field.syntax().text_range(),
                        );
                        self.add_def(scope, def);
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
                    let def = self.named_definition(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Binding,
                        name_tok.text_range(),
                        rp.syntax().text_range(),
                    );
                    self.add_def(scope, def);
                }
            }
            Pattern::Variant(vp) => {
                if let Some(sp) = vp.fields() {
                    for field in sp.fields() {
                        if let Some(sub_pat) = field.pattern() {
                            self.collect_pattern_bindings(scope, &sub_pat);
                        } else if let Some(name_tok) = field.name() {
                            let def = self.named_definition(
                                scope,
                                name_tok.text().to_string(),
                                DefKind::Binding,
                                name_tok.text_range(),
                                field.syntax().text_range(),
                            );
                            self.add_def(scope, def);
                        }
                    }
                }
            }
            Pattern::At(at) => {
                if let Some(name_tok) = at.binding_name() {
                    let def = self.named_definition(
                        scope,
                        name_tok.text().to_string(),
                        DefKind::Binding,
                        name_tok.text_range(),
                        at.syntax().text_range(),
                    );
                    self.add_def(scope, def);
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

pub fn top_level_definitions(source: SourceFileRef) -> Vec<Definition> {
    let tree = ScopeTree::build_source(source);
    tree.definitions(ScopeId::ROOT).to_vec()
}
