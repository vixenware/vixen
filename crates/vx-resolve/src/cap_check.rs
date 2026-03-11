//! Capability checking for Vixen source files.
//!
//! r[cap.model]
//!
//! Infers the capability fingerprint of closures from the callable stdlib/runtime surface,
//! and checks that closures passed to capability-masked callback slots satisfy
//! the mask.

use std::collections::HashMap;

use rowan::TextRange;
use vx_parser::{
    AstNode, CallExpr, ClosureExpr, Expr, FieldExpr, FnDecl, Item, NameRefExpr, SourceFile, Stmt,
    SyntaxKind,
};

use vx_types::{Capability, FnSig};

use crate::type_collect::TypeCollection;
use crate::{NameResolution, SourceSpan, TypeCheckResult};

/// Derive a CapSet from the capabilities on a function type.
fn caps_from_fn_ty_capabilities(caps: &[Capability]) -> CapSet {
    let mut result = CapSet::EMPTY;
    for cap in caps {
        if let Some(c) = CapSet::from_name(&cap.name) {
            result = result.union(c);
        }
    }
    result
}

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

/// Run capability checking on a parsed source file.
pub fn check_capabilities(
    file: &SourceFile,
    resolution: &NameResolution,
    types: &TypeCollection,
    type_check: &TypeCheckResult,
) -> CapCheckResult {
    let mut checker = CapChecker {
        resolution,
        types,
        type_check,
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
    resolution: &'a NameResolution,
    types: &'a TypeCollection,
    type_check: &'a TypeCheckResult,
    /// Maps function names to their declared capability masks.
    fn_caps: HashMap<String, CapSet>,
    errors: Vec<CapError>,
}

impl<'a> CapChecker<'a> {
    fn span(&self, range: TextRange) -> SourceSpan {
        SourceSpan::new(self.resolution.source, range)
    }

    fn is_closure_expr(expr: &Expr) -> bool {
        match expr {
            Expr::Closure(_) => true,
            Expr::Paren(paren) => paren
                .expr()
                .is_some_and(|inner| Self::is_closure_expr(&inner)),
            _ => false,
        }
    }

    fn resolved_def_for_name_ref(&self, name_ref: &NameRefExpr) -> Option<&crate::Definition> {
        let token = name_ref.token()?;
        let span = self.span(token.text_range());
        let resolved = self
            .resolution
            .refs
            .iter()
            .find(|resolved| resolved.span == span)?;
        resolved.target.as_ref().ok()
    }

    fn sig_for_name_ref(&self, name_ref: &NameRefExpr) -> Option<FnSig> {
        let def = self.resolved_def_for_name_ref(name_ref)?;
        (def.kind == crate::DefKind::Function)
            .then(|| self.types.fn_sig_for_def(def).cloned())
            .flatten()
    }

    fn sig_for_field_call(&self, field_expr: &FieldExpr) -> Option<FnSig> {
        let field_name = field_expr.field_name()?;
        let def = self
            .type_check
            .member_defs
            .get(&self.span(field_name.text_range()))?;
        let mut sig = self.types.fn_sig_for_def(def)?.clone();
        if sig
            .positional_params
            .first()
            .is_some_and(|param| param.name == "self")
        {
            sig.positional_params.remove(0);
        }
        Some(sig)
    }

    fn sig_for_callee(&self, callee: &Expr) -> Option<FnSig> {
        match callee {
            Expr::NameRef(name_ref) => self.sig_for_name_ref(name_ref),
            Expr::Field(field_expr) => self.sig_for_field_call(field_expr),
            _ => None,
        }
    }

    fn caps_for_callee(&self, callee: &Expr) -> Option<CapSet> {
        let sig = self.sig_for_callee(callee)?;
        Some(caps_from_fn_ty_capabilities(&sig.capabilities))
    }

    fn callee_label(&self, callee: &Expr) -> Option<String> {
        match callee {
            Expr::NameRef(name_ref) => Some(name_ref.token()?.text().to_string()),
            Expr::Field(field_expr) => Some(field_expr.field_name()?.text().to_string()),
            _ => None,
        }
    }

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
                if call
                    .syntax()
                    .parent()
                    .is_none_or(|parent| parent.kind() != SyntaxKind::WHERE_EXPR)
                {
                    self.check_call_callbacks(call);
                }
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
            Expr::Where(where_expr) => {
                self.check_where_callbacks(where_expr);
                if let Some(callee) = where_expr.callee() {
                    self.check_expr_for_callbacks(&callee);
                }
                if let Some(keywords) = where_expr.keywords() {
                    self.check_expr_for_callbacks(&keywords);
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

    fn check_call_callbacks(&mut self, call: &CallExpr) {
        let Some(callee) = call.callee() else {
            return;
        };
        let Some(callee_name) = self.callee_label(&callee) else {
            return;
        };
        let Some(sig) = self.sig_for_callee(&callee) else {
            return;
        };

        // Check positional args against function-typed parameters
        for (arg, param) in call.args().zip(sig.positional_params.iter()) {
            if let vx_types::Ty::Function(fn_ty) = &param.ty
                && let Some(arg_expr) = arg.expr()
            {
                let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                self.check_closure_mask(
                    &arg_expr,
                    mask,
                    &format!("{} {}", callee_name, param.name),
                );
            }
        }

        // Check keyword args against keyword-typed parameters
        for kw in call.keyword_args() {
            let Some(kw_name) = kw.name() else { continue };
            let kw_param = sig.keyword_params.iter().find(|p| p.name == kw_name.text());
            if let Some(kw_param) = kw_param
                && let vx_types::Ty::Function(fn_ty) = &kw_param.ty
                && let Some(arg_expr) = kw.expr()
            {
                let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                self.check_closure_mask(
                    &arg_expr,
                    mask,
                    &format!("{} {}", callee_name, kw_param.name),
                );
            }
        }

        // Also check positional args passed via keyword name
        for kw in call.keyword_args() {
            let Some(kw_name) = kw.name() else { continue };
            let pos_param = sig
                .positional_params
                .iter()
                .find(|p| p.name == kw_name.text());
            if let Some(pos_param) = pos_param
                && let vx_types::Ty::Function(fn_ty) = &pos_param.ty
                && let Some(arg_expr) = kw.expr()
            {
                let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                self.check_closure_mask(
                    &arg_expr,
                    mask,
                    &format!("{} {}", callee_name, pos_param.name),
                );
            }
        }
    }

    fn check_where_callbacks(&mut self, where_expr: &vx_parser::WhereExpr) {
        let (callee_expr, positional_args) = match where_expr.callee() {
            Some(Expr::Call(call)) => (call.callee(), Some(call.args().collect::<Vec<_>>())),
            Some(expr) => (Some(expr), None),
            None => (None, None),
        };
        let Some(callee_expr) = callee_expr else {
            return;
        };
        let Some(callee_name) = self.callee_label(&callee_expr) else {
            return;
        };
        let Some(sig) = self.sig_for_callee(&callee_expr) else {
            return;
        };

        if let Some(positional_args) = positional_args {
            for (arg, param) in positional_args.iter().zip(sig.positional_params.iter()) {
                if let vx_types::Ty::Function(fn_ty) = &param.ty
                    && let Some(arg_expr) = arg.expr()
                {
                    let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                    self.check_closure_mask(
                        &arg_expr,
                        mask,
                        &format!("{} {}", callee_name, param.name),
                    );
                }
            }
        }

        let Some(Expr::Record(record)) = where_expr.keywords() else {
            return;
        };

        for field in record.fields() {
            let Some(field_name) = field.name().map(|token| token.text().to_string()) else {
                continue;
            };
            let Some(arg_expr) = field.value() else {
                continue;
            };

            let kw_param = sig
                .keyword_params
                .iter()
                .find(|param| param.name == field_name);
            if let Some(kw_param) = kw_param
                && let vx_types::Ty::Function(fn_ty) = &kw_param.ty
            {
                let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                self.check_closure_mask(
                    &arg_expr,
                    mask,
                    &format!("{} {}", callee_name, kw_param.name),
                );
                continue;
            }

            let pos_param = sig
                .positional_params
                .iter()
                .find(|param| param.name == field_name);
            if let Some(pos_param) = pos_param
                && let vx_types::Ty::Function(fn_ty) = &pos_param.ty
            {
                let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                self.check_closure_mask(
                    &arg_expr,
                    mask,
                    &format!("{} {}", callee_name, pos_param.name),
                );
            }
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
            Expr::Paren(paren) => paren
                .expr()
                .map(|expr| self.infer_expr_caps(&expr))
                .unwrap_or(CapSet::EMPTY),
            Expr::NameRef(name_ref) => {
                // If this references a function with declared caps, propagate them
                if let Some(tok) = name_ref.token()
                    && let Some(&caps) = self.fn_caps.get(tok.text())
                {
                    return caps;
                }
                if let Some(sig) = self.sig_for_name_ref(name_ref) {
                    return caps_from_fn_ty_capabilities(&sig.capabilities);
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
            Expr::Paren(paren) => paren
                .expr()
                .map(|expr| self.infer_body_caps(&expr))
                .unwrap_or(CapSet::EMPTY),
            Expr::Call(call) => self.infer_call_caps(call),
            Expr::Invoke(invoke) => {
                let mut caps = CapSet::EMPTY;
                if let Some(callee) = invoke.callee() {
                    caps = caps.union(self.infer_body_caps(&callee));
                    if let Some(callee_caps) = self.caps_for_callee(&callee) {
                        caps = caps.union(callee_caps);
                    }
                }
                caps
            }
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
                if let Some(tok) = name_ref.token()
                    && let Some(&caps) = self.fn_caps.get(tok.text())
                {
                    return caps;
                }
                if let Some(sig) = self.sig_for_name_ref(name_ref) {
                    return caps_from_fn_ty_capabilities(&sig.capabilities);
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
            if let Some(callee_caps) = self.caps_for_callee(&callee) {
                caps = caps.union(callee_caps);
            }
            if let Expr::NameRef(name_ref) = &callee
                && let Some(tok) = name_ref.token()
                && let Some(&fn_caps) = self.fn_caps.get(tok.text())
            {
                caps = caps.union(fn_caps);
            }
        }

        // Capabilities from arguments
        for arg in call.args() {
            if let Some(arg_expr) = arg.expr() {
                if Self::is_closure_expr(&arg_expr) {
                    continue;
                }
                caps = caps.union(self.infer_body_caps(&arg_expr));
            }
        }
        for kw_arg in call.keyword_args() {
            if let Some(arg_expr) = kw_arg.expr() {
                if Self::is_closure_expr(&arg_expr) {
                    continue;
                }
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
