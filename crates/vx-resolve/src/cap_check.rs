//! Capability checking for Vixen source files.
//!
//! r[cap.model]
//!
//! Infers the capability fingerprint of closures from the builtins they call,
//! and checks that closures passed to capability-masked callback slots satisfy
//! the mask.

use std::collections::HashMap;

use rowan::TextRange;
use vx_parser::{AstNode, CallExpr, ClosureExpr, Expr, FnDecl, Item, SourceFile, Stmt};

use crate::scope::ScopeTree;
use crate::type_collect::TypeCollection;

/// A set of capabilities, represented as a bitset.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct CapSet(u8);

impl CapSet {
    pub const EMPTY: CapSet = CapSet(0);
    pub const FS: CapSet = CapSet(1 << 0);
    pub const DISCOVER: CapSet = CapSet(1 << 1);
    pub const SANDBOX: CapSet = CapSet(1 << 2);
    pub const HOST: CapSet = CapSet(1 << 3);

    pub fn from_name(name: &str) -> Option<CapSet> {
        match name {
            "fs" => Some(Self::FS),
            "discover" => Some(Self::DISCOVER),
            "sandbox" => Some(Self::SANDBOX),
            "host" => Some(Self::HOST),
            _ => None,
        }
    }

    pub fn union(self, other: CapSet) -> CapSet {
        CapSet(self.0 | other.0)
    }

    /// Returns true if `self` is a subset of `mask`.
    pub fn satisfies(self, mask: CapSet) -> CapSet {
        // Returns the excess capabilities (bits in self but not in mask)
        CapSet(self.0 & !mask.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn names(self) -> Vec<&'static str> {
        let mut result = Vec::new();
        if self.0 & Self::FS.0 != 0 {
            result.push("fs");
        }
        if self.0 & Self::DISCOVER.0 != 0 {
            result.push("discover");
        }
        if self.0 & Self::SANDBOX.0 != 0 {
            result.push("sandbox");
        }
        if self.0 & Self::HOST.0 != 0 {
            result.push("host");
        }
        result
    }
}

impl std::fmt::Display for CapSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self.names();
        if names.is_empty() {
            write!(f, "(none)")
        } else {
            for (i, name) in names.iter().enumerate() {
                if i > 0 {
                    write!(f, " ")?;
                }
                write!(f, "#{name}")?;
            }
            Ok(())
        }
    }
}

/// A capability error found during checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapError {
    pub range: TextRange,
    pub kind: CapErrorKind,
}

/// The kind of capability error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapErrorKind {
    /// A closure's inferred capabilities exceed the mask of the callback slot.
    CapabilityViolation {
        slot_name: String,
        allowed: CapSet,
        found: CapSet,
        excess: CapSet,
    },
    /// An unknown capability tag was used.
    UnknownCapability { name: String },
}

/// Result of capability checking.
#[derive(Debug)]
pub struct CapCheckResult {
    pub errors: Vec<CapError>,
}

/// Known builtin functions and their required capabilities.
fn builtin_caps(name: &str) -> Option<CapSet> {
    match name {
        // r[cap.kinds] — fs operations
        "read_file" | "read_dir" | "glob" | "exists" | "metadata" => Some(CapSet::FS),
        // r[exec.discover] — discovery operations
        "discover_exec" => Some(CapSet::DISCOVER),
        // r[exec.verifier.host] — host introspection
        "which" | "hash" => Some(CapSet::HOST),
        _ => None,
    }
}

/// Run capability checking on a parsed source file.
pub fn check_capabilities(
    file: &SourceFile,
    scopes: &ScopeTree,
    types: &TypeCollection,
) -> CapCheckResult {
    let mut checker = CapChecker {
        scopes,
        types,
        fn_caps: HashMap::new(),
        errors: Vec::new(),
    };

    // First pass: collect declared function capabilities
    for item in file.items() {
        if let Item::FnDecl(fn_decl) = &item
            && let Some(name_tok) = fn_decl.name()
        {
            let caps = caps_from_fn_decl(fn_decl);
            checker.fn_caps.insert(name_tok.text().to_string(), caps);
        }
    }

    // Second pass: check callback slots
    for item in file.items() {
        if let Item::FnDecl(fn_decl) = item
            && let Some(body) = fn_decl.body()
        {
            checker.check_stmts_for_callbacks(&body);
        }
    }

    // Third pass: validate capability tags
    for item in file.items() {
        if let Item::FnDecl(fn_decl) = &item {
            checker.validate_cap_tags_on_fn(fn_decl);
        }
    }

    CapCheckResult {
        errors: checker.errors,
    }
}

/// Extract the declared capability mask from a function declaration.
fn caps_from_fn_decl(fn_decl: &FnDecl) -> CapSet {
    let Some(cap_list) = fn_decl.capability_list() else {
        return CapSet::EMPTY;
    };
    let mut caps = CapSet::EMPTY;
    for tag in cap_list.tags() {
        if let Some(name_tok) = tag.name()
            && let Some(cap) = CapSet::from_name(name_tok.text())
        {
            caps = caps.union(cap);
        }
    }
    caps
}

struct CapChecker<'a> {
    #[allow(dead_code)]
    scopes: &'a ScopeTree,
    #[allow(dead_code)]
    types: &'a TypeCollection,
    /// Maps function names to their declared capability masks.
    fn_caps: HashMap<String, CapSet>,
    errors: Vec<CapError>,
}

impl<'a> CapChecker<'a> {
    fn check_stmts_for_callbacks(&mut self, block: &vx_parser::BlockExpr) {
        for stmt in block.statements() {
            self.check_stmt_for_callbacks(&stmt);
        }
    }

    fn check_stmt_for_callbacks(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(let_stmt) => {
                if let Some(expr) = let_stmt.expr() {
                    self.check_expr_for_callbacks(&expr);
                }
            }
            Stmt::Expr(expr) => {
                self.check_expr_for_callbacks(expr);
            }
            Stmt::Return(ret) => {
                if let Some(expr) = ret.expr() {
                    self.check_expr_for_callbacks(&expr);
                }
            }
            Stmt::FnDecl(fn_decl) => {
                if let Some(body) = fn_decl.body() {
                    self.check_stmts_for_callbacks(&body);
                }
            }
            _ => {}
        }
    }

    fn check_expr_for_callbacks(&mut self, expr: &Expr) {
        match expr {
            Expr::Call(call) => {
                self.check_call_callbacks(call);
                // Also recurse into the call's sub-expressions
                if let Some(callee) = call.callee() {
                    self.check_expr_for_callbacks(&callee);
                }
                for arg in call.args() {
                    if let Some(arg_expr) = arg.expr() {
                        self.check_expr_for_callbacks(&arg_expr);
                    }
                }
                for kw_arg in call.keyword_args() {
                    if let Some(arg_expr) = kw_arg.expr() {
                        self.check_expr_for_callbacks(&arg_expr);
                    }
                }
            }
            Expr::Block(block) => {
                self.check_stmts_for_callbacks(block);
            }
            Expr::Closure(closure) => {
                if let Some(body) = closure.body() {
                    self.check_expr_for_callbacks(&body);
                }
            }
            Expr::If(if_expr) => {
                if let Some(cond) = if_expr.condition() {
                    self.check_expr_for_callbacks(&cond);
                }
                if let Some(then_br) = if_expr.then_branch() {
                    self.check_expr_for_callbacks(&then_br);
                }
                if let Some(else_br) = if_expr.else_branch() {
                    self.check_expr_for_callbacks(&else_br);
                }
            }
            Expr::Match(match_expr) => {
                for arm in match_expr.arms() {
                    if let Some(body) = arm.expr() {
                        self.check_expr_for_callbacks(&body);
                    }
                }
            }
            _ => {
                // Recurse into child expressions
                for child in expr.syntax().children() {
                    if let Some(child_expr) = Expr::cast(child) {
                        self.check_expr_for_callbacks(&child_expr);
                    }
                }
            }
        }
    }

    /// Check a call expression for known callback-accepting builtins.
    fn check_call_callbacks(&mut self, call: &CallExpr) {
        let callee_name = match call.callee() {
            Some(Expr::NameRef(name_ref)) => name_ref.token().map(|t| t.text().to_string()),
            _ => None,
        };

        let Some(callee_name) = callee_name else {
            return;
        };

        match callee_name.as_str() {
            // r[eval.entry] — build(entry: Fn(...) #fs -> T)
            "build" => {
                // First positional arg is the entry closure
                if let Some(first_arg) = call.args().next()
                    && let Some(arg_expr) = first_arg.expr()
                {
                    self.check_closure_mask(&arg_expr, CapSet::FS, "build entry");
                }
            }
            // r[stabilize.signature] — stabilize(witness: Fn() #fs -> W, resolve: Fn(W) #discover -> T)
            "stabilize" => {
                let args: Vec<_> = call.args().collect();
                if let Some(witness_arg) = args.first()
                    && let Some(arg_expr) = witness_arg.expr()
                {
                    self.check_closure_mask(&arg_expr, CapSet::FS, "stabilize witness");
                }
                if let Some(resolve_arg) = args.get(1)
                    && let Some(arg_expr) = resolve_arg.expr()
                {
                    self.check_closure_mask(&arg_expr, CapSet::DISCOVER, "stabilize resolve");
                }
                // Also check keyword args
                for kw in call.keyword_args() {
                    if let Some(name) = kw.name() {
                        let mask = match name.text() {
                            "witness" => Some(CapSet::FS),
                            "resolve" => Some(CapSet::DISCOVER),
                            _ => None,
                        };
                        if let Some(mask) = mask
                            && let Some(arg_expr) = kw.expr()
                        {
                            self.check_closure_mask(
                                &arg_expr,
                                mask,
                                &format!("stabilize {}", name.text()),
                            );
                        }
                    }
                }
            }
            // r[exec.signature] — exec(...; observer: Fn(Process, Tree) #sandbox -> Stream[T],
            //                              verifier: Fn(Host) #host -> ())
            "exec" => {
                for kw in call.keyword_args() {
                    if let Some(name) = kw.name() {
                        let mask = match name.text() {
                            "observer" => Some(CapSet::SANDBOX),
                            "verifier" => Some(CapSet::HOST),
                            _ => None,
                        };
                        if let Some(mask) = mask
                            && let Some(arg_expr) = kw.expr()
                        {
                            self.check_closure_mask(
                                &arg_expr,
                                mask,
                                &format!("exec {}", name.text()),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Check that a closure expression's inferred capabilities satisfy a mask.
    fn check_closure_mask(&mut self, expr: &Expr, mask: CapSet, slot_name: &str) {
        let inferred = self.infer_expr_caps(expr);
        let excess = inferred.satisfies(mask);
        if !excess.is_empty() {
            self.errors.push(CapError {
                range: expr.syntax().text_range(),
                kind: CapErrorKind::CapabilityViolation {
                    slot_name: slot_name.to_string(),
                    allowed: mask,
                    found: inferred,
                    excess,
                },
            });
        }
    }

    /// Infer the capability fingerprint of an expression.
    fn infer_expr_caps(&self, expr: &Expr) -> CapSet {
        match expr {
            Expr::Closure(closure) => self.infer_closure_caps(closure),
            Expr::NameRef(name_ref) => {
                // If this references a function with declared caps, propagate them
                if let Some(tok) = name_ref.token() {
                    let name = tok.text();
                    if let Some(&caps) = self.fn_caps.get(name) {
                        return caps;
                    }
                    if let Some(caps) = builtin_caps(name) {
                        return caps;
                    }
                }
                CapSet::EMPTY
            }
            _ => CapSet::EMPTY,
        }
    }

    /// Infer the capability fingerprint of a closure from its body.
    fn infer_closure_caps(&self, closure: &ClosureExpr) -> CapSet {
        let Some(body) = closure.body() else {
            return CapSet::EMPTY;
        };
        self.infer_body_caps(&body)
    }

    /// Infer capabilities from an expression (closure body or block).
    fn infer_body_caps(&self, expr: &Expr) -> CapSet {
        match expr {
            Expr::Block(block) => {
                let mut caps = CapSet::EMPTY;
                for stmt in block.statements() {
                    caps = caps.union(self.infer_stmt_caps(&stmt));
                }
                caps
            }
            Expr::Call(call) => self.infer_call_caps(call),
            Expr::If(if_expr) => {
                let mut caps = CapSet::EMPTY;
                if let Some(cond) = if_expr.condition() {
                    caps = caps.union(self.infer_body_caps(&cond));
                }
                if let Some(then_br) = if_expr.then_branch() {
                    caps = caps.union(self.infer_body_caps(&then_br));
                }
                if let Some(else_br) = if_expr.else_branch() {
                    caps = caps.union(self.infer_body_caps(&else_br));
                }
                caps
            }
            Expr::Match(match_expr) => {
                let mut caps = CapSet::EMPTY;
                for arm in match_expr.arms() {
                    if let Some(body) = arm.expr() {
                        caps = caps.union(self.infer_body_caps(&body));
                    }
                }
                caps
            }
            Expr::Closure(inner) => {
                // A nested closure captures authority — propagate its caps
                self.infer_closure_caps(inner)
            }
            Expr::NameRef(name_ref) => {
                if let Some(tok) = name_ref.token() {
                    let name = tok.text();
                    if let Some(&caps) = self.fn_caps.get(name) {
                        return caps;
                    }
                    if let Some(caps) = builtin_caps(name) {
                        return caps;
                    }
                }
                CapSet::EMPTY
            }
            _ => {
                // Recurse into child expressions
                let mut caps = CapSet::EMPTY;
                for child in expr.syntax().children() {
                    if let Some(child_expr) = Expr::cast(child) {
                        caps = caps.union(self.infer_body_caps(&child_expr));
                    }
                }
                caps
            }
        }
    }

    fn infer_stmt_caps(&self, stmt: &Stmt) -> CapSet {
        match stmt {
            Stmt::Let(let_stmt) => let_stmt
                .expr()
                .map(|e| self.infer_body_caps(&e))
                .unwrap_or(CapSet::EMPTY),
            Stmt::Expr(expr) => self.infer_body_caps(expr),
            Stmt::Return(ret) => ret
                .expr()
                .map(|e| self.infer_body_caps(&e))
                .unwrap_or(CapSet::EMPTY),
            Stmt::FnDecl(_) => {
                // Local function body capabilities don't propagate to enclosing scope
                CapSet::EMPTY
            }
            _ => CapSet::EMPTY,
        }
    }

    /// Infer capabilities from a call expression.
    fn infer_call_caps(&self, call: &CallExpr) -> CapSet {
        let mut caps = CapSet::EMPTY;

        // Capabilities from the callee
        if let Some(callee) = call.callee() {
            match &callee {
                Expr::NameRef(name_ref) => {
                    if let Some(tok) = name_ref.token() {
                        let name = tok.text();
                        if let Some(builtin) = builtin_caps(name) {
                            caps = caps.union(builtin);
                        }
                        if let Some(&fn_caps) = self.fn_caps.get(name) {
                            caps = caps.union(fn_caps);
                        }
                    }
                }
                Expr::Field(field_expr) => {
                    // Method calls like host.which(...) — check method name
                    if let Some(field_name) = field_expr.field_name()
                        && let Some(builtin) = builtin_caps(field_name.text())
                    {
                        caps = caps.union(builtin);
                    }
                }
                _ => {}
            }
        }

        // Capabilities from arguments
        for arg in call.args() {
            if let Some(arg_expr) = arg.expr() {
                caps = caps.union(self.infer_body_caps(&arg_expr));
            }
        }
        for kw_arg in call.keyword_args() {
            if let Some(arg_expr) = kw_arg.expr() {
                caps = caps.union(self.infer_body_caps(&arg_expr));
            }
        }

        caps
    }

    /// Validate that capability tags on function declarations use known names.
    fn validate_cap_tags_on_fn(&mut self, fn_decl: &FnDecl) {
        if let Some(cap_list) = fn_decl.capability_list() {
            for tag in cap_list.tags() {
                if let Some(name_tok) = tag.name()
                    && CapSet::from_name(name_tok.text()).is_none()
                {
                    self.errors.push(CapError {
                        range: tag.syntax().text_range(),
                        kind: CapErrorKind::UnknownCapability {
                            name: name_tok.text().to_string(),
                        },
                    });
                }
            }
        }

        // Recurse into body for local fn declarations
        if let Some(body) = fn_decl.body() {
            self.validate_cap_tags_in_block(&body);
        }
    }

    fn validate_cap_tags_in_block(&mut self, block: &vx_parser::BlockExpr) {
        for stmt in block.statements() {
            if let Stmt::FnDecl(fn_decl) = stmt {
                self.validate_cap_tags_on_fn(&fn_decl);
            }
        }
    }
}
