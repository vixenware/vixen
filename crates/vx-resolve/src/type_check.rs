//! Expression typing pass for Vixen source files.
//!
//! A narrow, forward-only pass that infers types for expressions in the
//! current file. Not a full Hindley-Milner engine — just enough to provide
//! hover info and catch obvious type errors in builtin calls.

use std::collections::HashMap;

use rowan::TextRange;
use vx_parser::{
    ArrayExpr, AstNode, BinaryExpr, BlockExpr, CallExpr, ClosureExpr, Expr, FallibleExpr,
    FieldExpr, IfExpr, InvokeExpr, LetStmt, LiteralExpr, MapExpr, MatchExpr, NameRefExpr,
    PartialAppExpr, Pattern, PrefixExpr, RecordExpr, SourceFile, Stmt, StructLiteralExpr,
    SyntaxKind, WhereExpr, WithExpr,
};
use vx_types::{Capability, FnSig, ParamDef, Ty};

use crate::SourceSpan;
use crate::resolve::NameResolution;
use crate::scope::{DefKind, Definition};
use crate::type_collect::TypeCollection;
use crate::{resolve_member_for_receiver, resolve_namespace_members};

/// A type error found during expression typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    pub range: TextRange,
    pub kind: TypeErrorKind,
}

/// The kind of type error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeErrorKind {
    /// Argument type doesn't match parameter type.
    ArgTypeMismatch {
        param_name: String,
        expected: Ty,
        found: Ty,
    },
    /// Method not found on type.
    NoSuchMethod {
        receiver_ty: Ty,
        method_name: String,
    },
}

/// Result of expression typing.
#[derive(Debug)]
pub struct TypeCheckResult {
    /// Map from expression text ranges to their inferred types.
    pub expr_types: HashMap<SourceSpan, Ty>,
    /// Real member-definition targets for field/method references.
    pub member_defs: HashMap<SourceSpan, Definition>,
    /// Types for closure parameters inferred from callback slots.
    pub closure_param_types: HashMap<SourceSpan, Ty>,
    /// Documentation inherited from callback slots for closure parameters.
    pub closure_param_docs: HashMap<SourceSpan, String>,
    /// Map from binding name ranges to their inferred types.
    pub binding_types: HashMap<SourceSpan, Ty>,
    /// Type errors found during checking.
    pub errors: Vec<TypeError>,
}

#[derive(Clone)]
struct BoundMethodSig {
    params: Vec<ParamDef>,
    ret: Ty,
    capabilities: Vec<Capability>,
}

/// Run expression typing on a parsed source file.
pub fn type_check(
    file: &SourceFile,
    resolution: &NameResolution,
    types: &TypeCollection,
) -> TypeCheckResult {
    let mut checker = ExprTyper {
        file,
        resolution,
        types,
        expr_types: HashMap::new(),
        member_defs: HashMap::new(),
        errors: Vec::new(),
        closure_param_types: HashMap::new(),
        closure_param_docs: HashMap::new(),
        pipe_placeholder_types: HashMap::new(),
        binding_types: HashMap::new(),
    };

    for stmt in file.statements() {
        checker.check_stmt(&stmt);
    }

    TypeCheckResult {
        expr_types: checker.expr_types,
        member_defs: checker.member_defs,
        closure_param_types: checker.closure_param_types,
        closure_param_docs: checker.closure_param_docs,
        binding_types: checker.binding_types,
        errors: checker.errors,
    }
}

struct ExprTyper<'a> {
    file: &'a SourceFile,
    resolution: &'a NameResolution,
    types: &'a TypeCollection,
    expr_types: HashMap<SourceSpan, Ty>,
    member_defs: HashMap<SourceSpan, Definition>,
    errors: Vec<TypeError>,
    /// Types for closure parameters, populated when we see a closure in a
    /// known callback slot (e.g., exec observer's proc and tree params).
    closure_param_types: HashMap<SourceSpan, Ty>,
    /// Documentation for closure parameters inherited from the callback slot.
    closure_param_docs: HashMap<SourceSpan, String>,
    /// Types for `_` placeholders bound by the current pipe stage.
    pipe_placeholder_types: HashMap<SourceSpan, Ty>,
    /// Types for let bindings, keyed by the binding's name_range.
    binding_types: HashMap<SourceSpan, Ty>,
}

impl<'a> ExprTyper<'a> {
    fn span(&self, range: TextRange) -> SourceSpan {
        SourceSpan::new(self.resolution.source, range)
    }

    fn push_error(&mut self, error: TypeError) {
        if !self.errors.contains(&error) {
            self.errors.push(error);
        }
    }

    fn record_member_def(&mut self, field: &FieldExpr, def: Definition) {
        let Some(field_name) = field.field_name() else {
            return;
        };
        self.member_defs
            .insert(self.span(field_name.text_range()), def);
    }

    fn peel_parens(expr: Expr) -> Expr {
        let mut current = expr;
        while let Expr::Paren(ref paren) = current {
            let Some(inner) = paren.expr() else {
                break;
            };
            current = inner;
        }
        current
    }

    fn check_block(&mut self, block: &BlockExpr) {
        for stmt in block.statements() {
            self.check_stmt(&stmt);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(let_stmt) => {
                // Check for explicit type annotation: let x: Foo = ...
                let annotation_ty = let_stmt
                    .slot_type()
                    .and_then(|st| st.ty())
                    .map(|t| self.types.resolve_type_annotation(&t))
                    .filter(|t| !t.is_poison());

                if let Some(expr) = let_stmt.expr() {
                    let inferred_ty = self.infer_expr(&expr);
                    // Prefer the annotation type if present, otherwise use inferred
                    let ty = annotation_ty.unwrap_or(inferred_ty);
                    if !ty.is_poison() {
                        if let Some(name_tok) = let_stmt.name() {
                            self.binding_types
                                .insert(self.span(name_tok.text_range()), ty);
                        }
                    }
                } else if let Some(ty) = annotation_ty {
                    if let Some(name_tok) = let_stmt.name() {
                        self.binding_types
                            .insert(self.span(name_tok.text_range()), ty);
                    }
                }
            }
            Stmt::Expr(expr) => {
                self.infer_expr(expr);
            }
            Stmt::Return(ret) => {
                if let Some(expr) = ret.expr() {
                    self.infer_expr(&expr);
                }
            }
            Stmt::FnDecl(fn_decl) => {
                if let Some(body) = fn_decl.body() {
                    self.check_block(&body);
                }
            }
            _ => {}
        }
    }

    /// Infer the type of an expression and record it.
    fn infer_expr(&mut self, expr: &Expr) -> Ty {
        let ty = match expr {
            Expr::Literal(lit) => self.infer_literal(lit),
            Expr::PathLiteral(_) => self.nominal_ty("Path"),
            Expr::NameRef(name_ref) => self.infer_name_ref(name_ref),
            Expr::Paren(paren) => paren
                .expr()
                .map(|expr| self.infer_expr(&expr))
                .unwrap_or(Ty::Poison),
            Expr::PartialApp(partial) => self.infer_partial_app(partial),
            Expr::Call(call) => self.infer_call(call),
            Expr::Where(where_expr) => self.infer_where(where_expr),
            Expr::Invoke(invoke) => self.infer_invoke(invoke),
            Expr::Field(field) => self.infer_field(field),
            Expr::Block(block) => self.infer_block(block),
            Expr::If(if_expr) => self.infer_if(if_expr),
            Expr::Closure(closure) => self.infer_closure(closure),
            Expr::InterpolatedString(interp) => {
                // Infer types of interpolated expressions
                for child in interp.syntax().children() {
                    if let Some(e) = Expr::cast(child) {
                        self.infer_expr(&e);
                    }
                }
                Ty::Scalar(vx_types::Scalar::String)
            }
            Expr::Array(arr) => self.infer_array(arr),
            Expr::Record(record) => self.infer_record(record),
            Expr::Map(map) => self.infer_map(map),
            Expr::StructLiteral(sl) => self.infer_struct_literal(sl),
            Expr::Match(m) => self.infer_match(m),
            Expr::Binary(bin) => self.infer_binary(bin),
            Expr::Prefix(pre) => self.infer_prefix(pre),
            Expr::Fallible(fallible) => self.infer_fallible(fallible),
            Expr::With(w) => self.infer_with(w),
            _ => {
                // Recurse into child expressions
                for child in expr.syntax().children() {
                    if let Some(e) = Expr::cast(child) {
                        self.infer_expr(&e);
                    }
                }
                Ty::Poison
            }
        };

        if !ty.is_poison() {
            self.expr_types
                .insert(self.span(expr.syntax().text_range()), ty.clone());
        }
        ty
    }

    fn infer_literal(&self, lit: &LiteralExpr) -> Ty {
        let Some(token) = lit.token() else {
            return Ty::Poison;
        };
        match token.kind() {
            SyntaxKind::INT => Ty::Scalar(vx_types::Scalar::U64),
            SyntaxKind::FLOAT => Ty::Scalar(vx_types::Scalar::F64),
            SyntaxKind::STRING => Ty::Scalar(vx_types::Scalar::String),
            SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW => Ty::Scalar(vx_types::Scalar::Bool),
            _ => Ty::Poison,
        }
    }

    fn nominal_ty(&self, name: &str) -> Ty {
        let Some(&id) = self.types.env.lookup(name).first() else {
            return Ty::Poison;
        };

        match self.types.env.get(id) {
            vx_types::TypeDef::Struct(_) => Ty::Struct(vx_types::StructTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Enum(_) => Ty::Enum(vx_types::EnumTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Newtype(_) => Ty::Newtype(vx_types::NewtypeTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Alias(_) => Ty::Alias(vx_types::AliasTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Opaque(_) => Ty::Opaque(vx_types::OpaqueTy {
                id,
                type_args: vec![],
            }),
        }
    }

    fn infer_name_ref(&self, name_ref: &NameRefExpr) -> Ty {
        if let Some(token) = name_ref.token() {
            let span = self.span(token.text_range());
            if let Some(ty) = self.pipe_placeholder_types.get(&span) {
                return ty.clone();
            }
        }
        if let Some(def) = self.resolved_def_for_name_ref(name_ref) {
            if let Some(ty) = self.closure_param_types.get(&def.name_span) {
                return ty.clone();
            }
            return self.type_of_def(def);
        }

        Ty::Poison
    }

    fn resolved_def_for_name_ref(&self, name_ref: &NameRefExpr) -> Option<&Definition> {
        let token = name_ref.token()?;
        let ref_span = self.span(token.text_range());
        let resolved = self.resolution.refs.iter().find(|r| r.span == ref_span)?;
        resolved.target.as_ref().ok()
    }

    fn type_of_def(&self, def: &Definition) -> Ty {
        match def.kind {
            DefKind::Parameter => {
                // Check if this parameter has a type from closure_param_types
                if let Some(ty) = self.closure_param_types.get(&def.name_span) {
                    return ty.clone();
                }
                // Check function parameter types from FnSig
                if let Some(sig) = self.find_fn_sig_for_param(def) {
                    return sig;
                }
                Ty::Poison
            }
            DefKind::Function => {
                // Look up user-declared fn signature
                if let Some(sig) = self.types.fn_sig_for_def(def) {
                    return Ty::Function(vx_types::FunctionTy {
                        params: sig
                            .positional_params
                            .iter()
                            .map(|param| param.ty.clone())
                            .collect(),
                        ret: Box::new(sig.ret.clone()),
                        capabilities: sig.capabilities.clone(),
                    });
                }
                Ty::Poison
            }
            DefKind::Binding => {
                // Look up the type recorded when the let initializer was typed
                if let Some(ty) = self.binding_types.get(&def.name_span) {
                    return ty.clone();
                }
                Ty::Poison
            }
            _ => Ty::Poison,
        }
    }

    fn find_fn_sig_for_param(&self, def: &Definition) -> Option<Ty> {
        // Walk up the scope chain from the parameter's scope to find its
        // enclosing function scope, then look up the FnSig by matching the
        // function's name range inside the scope's text range.
        let Some(scope_range) = self.resolution.scopes.text_range(def.source(), def.scope) else {
            return None;
        };
        let Some(scope_kind) = self.resolution.scopes.scope_kind(def.source(), def.scope) else {
            return None;
        };

        // Parameters can be in Function or Closure scopes
        if scope_kind != crate::scope::ScopeKind::Function {
            return None;
        }

        // Find the FnSig whose name range is within this function scope's range
        for (site, sig) in &self.types.def_to_fn_sig {
            if site.source == def.source() && scope_range.contains_range(site.range) {
                // Match by position in positional params
                for (i, param) in sig.positional_params.iter().enumerate() {
                    if param.name == def.name {
                        if !param.ty.is_poison() {
                            return Some(param.ty.clone());
                        }
                        // Also try matching by index if names differ
                        let _ = i;
                    }
                }
                // Match keyword params
                for param in &sig.keyword_params {
                    if param.name == def.name && !param.ty.is_poison() {
                        return Some(param.ty.clone());
                    }
                }
            }
        }

        None
    }

    fn sig_for_name_ref(&self, name_ref: &NameRefExpr) -> Option<FnSig> {
        let def = self.resolved_def_for_name_ref(name_ref)?;
        match def.kind {
            DefKind::Function => self.types.fn_sig_for_def(def).cloned(),
            DefKind::Namespace => {
                let mut sigs = self.constructor_sigs_for_namespace(def);
                (sigs.len() == 1).then(|| sigs.swap_remove(0))
            }
            _ => None,
        }
    }

    fn sig_for_expr(&self, expr: &Expr) -> Option<FnSig> {
        match expr {
            Expr::NameRef(name_ref) => self.sig_for_name_ref(name_ref),
            Expr::PartialApp(partial) => partial
                .callee()
                .and_then(|callee| self.sig_for_expr(&callee)),
            Expr::Paren(paren) => paren.expr().and_then(|inner| self.sig_for_expr(&inner)),
            _ => None,
        }
    }

    fn function_ty_from_sig(&self, sig: &FnSig) -> Ty {
        Ty::Function(vx_types::FunctionTy {
            params: sig
                .positional_params
                .iter()
                .map(|param| param.ty.clone())
                .collect(),
            ret: Box::new(sig.ret.clone()),
            capabilities: sig.capabilities.clone(),
        })
    }

    fn constructor_sigs_for_namespace(&self, namespace: &Definition) -> Vec<FnSig> {
        resolve_namespace_members(&self.resolution.scopes, namespace, &namespace.name)
            .into_iter()
            .filter_map(|def| self.types.fn_sig_for_def(&def).cloned())
            .collect()
    }

    fn check_record_keyword_args(&mut self, record: &RecordExpr, sig: &FnSig) {
        for field in record.fields() {
            let Some(name) = field.name().map(|token| token.text().to_string()) else {
                continue;
            };
            let Some(arg_expr) = field.value() else {
                continue;
            };
            let param = sig
                .keyword_params
                .iter()
                .find(|param| param.name == name)
                .map(|param| &param.ty)
                .or_else(|| {
                    sig.positional_params
                        .iter()
                        .find(|param| param.name == name)
                        .map(|param| &param.ty)
                });
            let Some(param_ty) = param else {
                self.infer_expr(&arg_expr);
                continue;
            };
            let arg_ty = self.infer_expr(&arg_expr);
            if !arg_ty.is_poison() && !param_ty.is_poison() && !types_compatible(&arg_ty, param_ty)
            {
                self.push_error(TypeError {
                    range: arg_expr.syntax().text_range(),
                    kind: TypeErrorKind::ArgTypeMismatch {
                        param_name: name,
                        expected: param_ty.clone(),
                        found: arg_ty,
                    },
                });
            }
        }
    }

    fn specialize_sig_from_record(&mut self, sig: &FnSig, record: &RecordExpr) -> FnSig {
        if sig.type_params.is_empty() {
            return sig.clone();
        }

        let mut substitutions = vec![None; sig.type_params.len()];
        for field in record.fields() {
            let Some(name) = field.name().map(|token| token.text().to_string()) else {
                continue;
            };
            let Some(arg_expr) = field.value() else {
                continue;
            };
            let Some(param_ty) = sig
                .keyword_params
                .iter()
                .find(|param| param.name == name)
                .map(|param| &param.ty)
                .or_else(|| {
                    sig.positional_params
                        .iter()
                        .find(|param| param.name == name)
                        .map(|param| &param.ty)
                })
            else {
                continue;
            };

            let arg_ty = self.infer_expr(&arg_expr);
            infer_type_params(param_ty, &arg_ty, &mut substitutions);
        }

        let mut specialized = sig.clone();
        for param in &mut specialized.positional_params {
            param.ty = substitute_bound_type_params(&param.ty, &substitutions);
        }
        for param in &mut specialized.keyword_params {
            param.ty = substitute_bound_type_params(&param.ty, &substitutions);
        }
        specialized.ret = substitute_bound_type_params(&specialized.ret, &substitutions);
        specialized
    }

    fn populate_record_callback_param_types(&mut self, record: &RecordExpr, sig: &FnSig) {
        for field in record.fields() {
            let Some(name) = field.name().map(|token| token.text().to_string()) else {
                continue;
            };
            let Some(arg_expr) = field.value().map(Self::peel_parens) else {
                continue;
            };
            let Expr::Closure(closure) = arg_expr else {
                continue;
            };

            let kw_param = sig.keyword_params.iter().find(|param| param.name == name);
            if let Some(param) = kw_param
                && let vx_types::Ty::Function(fn_ty) = &param.ty
            {
                self.bind_closure_params(
                    &closure,
                    &fn_ty.params,
                    param.doc.as_deref(),
                    &param.name,
                );
                continue;
            }

            let pos_param = sig
                .positional_params
                .iter()
                .find(|param| param.name == name);
            if let Some(param) = pos_param
                && let vx_types::Ty::Function(fn_ty) = &param.ty
            {
                self.bind_closure_params(
                    &closure,
                    &fn_ty.params,
                    param.doc.as_deref(),
                    &param.name,
                );
            }
        }
    }

    fn infer_call(&mut self, call: &CallExpr) -> Ty {
        let Some(callee) = call.callee() else {
            return Ty::Poison;
        };

        match &callee {
            Expr::NameRef(name_ref) => {
                let callee_name = name_ref.token().map(|t| t.text().to_string());
                if let Some(sig) = self.call_sig_for_name_ref(call, name_ref) {
                    self.populate_callback_param_types_from_sig(call, &sig);
                    self.check_call_args(call, &sig);
                    self.check_keyword_args(call, &sig);
                    self.infer_extra_call_args(call, sig.positional_params.len());
                    if callee_name.as_deref() == Some("discover")
                        && let Some(discovered_ty) = self.infer_discover_call(call)
                    {
                        return discovered_ty;
                    }
                    return sig.ret.clone();
                }

                self.infer_call_arg_exprs(call);
                Ty::Poison
            }
            Expr::Field(field_expr) => {
                if let Some(sig) = self.infer_static_namespace_call_sig(call, field_expr) {
                    self.check_call_args(call, &sig);
                    self.check_keyword_args(call, &sig);
                    self.infer_extra_call_args(call, sig.positional_params.len());
                    self.infer_all_keyword_arg_exprs(call);
                    return sig.ret;
                }

                // Method call: receiver.method(args)
                let receiver_ty = field_expr
                    .receiver()
                    .map(|r| self.infer_expr(&r))
                    .unwrap_or(Ty::Poison);
                let method_name = field_expr
                    .field_name()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();

                if let Some(member_def) = self.resolve_method_member_def(field_expr, &receiver_ty) {
                    self.record_member_def(field_expr, member_def.clone());
                    if let Some(method_sig) =
                        self.bound_method_sig_for_call(&receiver_ty, &member_def, call)
                    {
                        self.check_method_args(call, &method_sig.params, &method_name);
                        self.infer_extra_call_args(call, method_sig.params.len());
                        self.infer_all_keyword_arg_exprs(call);
                        return method_sig.ret.clone();
                    }
                }

                self.push_error(TypeError {
                    range: field_expr.syntax().text_range(),
                    kind: TypeErrorKind::NoSuchMethod {
                        receiver_ty: receiver_ty.clone(),
                        method_name,
                    },
                });

                self.infer_call_arg_exprs(call);
                Ty::Poison
            }
            _ => {
                let callee_ty = self.infer_expr(&callee);
                self.infer_call_arg_exprs(call);
                match callee_ty {
                    Ty::Function(fn_ty) => (*fn_ty.ret).clone(),
                    _ => Ty::Poison,
                }
            }
        }
    }

    fn infer_discover_call(&mut self, call: &CallExpr) -> Option<Ty> {
        let probe = call.args().nth(1)?.expr().map(Self::peel_parens)?;
        match probe {
            Expr::Closure(closure) => {
                let body = closure.body()?;
                self.expr_types
                    .get(&self.span(body.syntax().text_range()))
                    .cloned()
            }
            _ => None,
        }
    }

    fn infer_call_arg_exprs(&mut self, call: &CallExpr) {
        for arg in call.args() {
            if let Some(arg_expr) = arg.expr() {
                self.infer_expr(&arg_expr);
            }
        }
        self.infer_all_keyword_arg_exprs(call);
    }

    fn infer_static_namespace_call_sig(
        &mut self,
        call: &CallExpr,
        field_expr: &FieldExpr,
    ) -> Option<FnSig> {
        let member_defs = self.resolve_static_member_defs(field_expr);
        let (member_def, sig) = self.select_overloaded_def_sig(call, &member_defs)?;
        self.record_member_def(field_expr, member_def);
        Some(self.instantiate_call_sig(call, sig))
    }

    fn instantiate_call_sig(&self, call: &CallExpr, mut sig: FnSig) -> FnSig {
        let Some(type_args) = call.type_args() else {
            return sig;
        };
        let substitutions: Vec<Ty> = type_args
            .types()
            .map(|ty| self.types.resolve_type_annotation(&ty))
            .collect();
        if substitutions.is_empty() {
            return sig;
        }

        for param in &mut sig.positional_params {
            param.ty = substitute_type_params(&param.ty, &substitutions);
        }
        for param in &mut sig.keyword_params {
            param.ty = substitute_type_params(&param.ty, &substitutions);
        }
        sig.ret = substitute_type_params(&sig.ret, &substitutions);
        sig
    }

    fn select_fn_sig(&mut self, call: &CallExpr, sigs: &[FnSig]) -> Option<FnSig> {
        if sigs.is_empty() {
            return None;
        }

        let arg_tys: Vec<Ty> = call
            .args()
            .filter_map(|arg| arg.expr())
            .map(|arg| self.infer_expr(&arg))
            .collect();

        sigs.iter()
            .find(|sig| call_matches(&arg_tys, sig))
            .cloned()
            .or_else(|| {
                (sigs.len() == 1 && arg_tys.len() == sigs[0].positional_params.len())
                    .then(|| sigs[0].clone())
            })
    }

    fn select_overloaded_def_sig(
        &mut self,
        call: &CallExpr,
        defs: &[Definition],
    ) -> Option<(Definition, FnSig)> {
        if defs.is_empty() {
            return None;
        }

        let arg_tys: Vec<Ty> = call
            .args()
            .filter_map(|arg| arg.expr())
            .map(|arg| self.infer_expr(&arg))
            .collect();

        let mut candidates: Vec<(Definition, FnSig)> = defs
            .iter()
            .filter_map(|def| {
                self.types
                    .fn_sig_for_def(def)
                    .cloned()
                    .map(|sig| (def.clone(), sig))
            })
            .collect();

        if let Some(index) = candidates
            .iter()
            .position(|(_, sig)| call_matches(&arg_tys, sig))
        {
            return Some(candidates.swap_remove(index));
        }

        (candidates.len() == 1 && arg_tys.len() == candidates[0].1.positional_params.len())
            .then(|| candidates.swap_remove(0))
    }

    fn infer_extra_call_args(&mut self, call: &CallExpr, checked_positional: usize) {
        for arg in call.args().skip(checked_positional) {
            if let Some(arg_expr) = arg.expr() {
                self.infer_expr(&arg_expr);
            }
        }
    }

    fn infer_all_keyword_arg_exprs(&mut self, call: &CallExpr) {
        for kw_arg in call.keyword_args() {
            if let Some(arg_expr) = kw_arg.expr() {
                self.infer_expr(&arg_expr);
            }
        }
    }

    fn populate_method_callback_param_types(
        &mut self,
        call: &CallExpr,
        params: &[vx_types::ParamDef],
    ) {
        for (arg, param) in call.args().zip(params.iter()) {
            if let vx_types::Ty::Function(fn_ty) = &param.ty {
                let arg_expr = arg.expr().map(Self::peel_parens);
                if let Some(Expr::Closure(closure)) = arg_expr {
                    self.bind_closure_params(
                        &closure,
                        &fn_ty.params,
                        param.doc.as_deref(),
                        &param.name,
                    );
                }
            }
        }
    }

    /// Populate closure parameter types for callback slots.
    /// Derives parameter types from the resolved call signature.
    fn populate_callback_param_types_from_sig(&mut self, call: &CallExpr, sig: &FnSig) {
        // Check positional args against function-typed parameters
        for (arg, param) in call.args().zip(sig.positional_params.iter()) {
            if let vx_types::Ty::Function(fn_ty) = &param.ty {
                let arg_expr = arg.expr().map(Self::peel_parens);
                if let Some(Expr::Closure(closure)) = arg_expr {
                    self.bind_closure_params(
                        &closure,
                        &fn_ty.params,
                        param.doc.as_deref(),
                        &param.name,
                    );
                }
            }
        }

        // Check keyword args against keyword-typed parameters
        for kw in call.keyword_args() {
            let Some(kw_name) = kw.name() else { continue };
            // Check keyword params
            let kw_param = sig.keyword_params.iter().find(|p| p.name == kw_name.text());
            if let Some(kw_param) = kw_param {
                if let vx_types::Ty::Function(fn_ty) = &kw_param.ty {
                    let kw_expr = kw.expr().map(Self::peel_parens);
                    if let Some(Expr::Closure(closure)) = kw_expr {
                        self.bind_closure_params(
                            &closure,
                            &fn_ty.params,
                            kw_param.doc.as_deref(),
                            &kw_param.name,
                        );
                    }
                }
            }
            // Also check positional params passed by keyword name
            let pos_param = sig
                .positional_params
                .iter()
                .find(|p| p.name == kw_name.text());
            if let Some(pos_param) = pos_param {
                if let vx_types::Ty::Function(fn_ty) = &pos_param.ty {
                    let kw_expr = kw.expr().map(Self::peel_parens);
                    if let Some(Expr::Closure(closure)) = kw_expr {
                        self.bind_closure_params(
                            &closure,
                            &fn_ty.params,
                            pos_param.doc.as_deref(),
                            &pos_param.name,
                        );
                    }
                }
            }
        }
    }

    fn call_sig_for_name_ref(&mut self, call: &CallExpr, name_ref: &NameRefExpr) -> Option<FnSig> {
        let def = self.resolved_def_for_name_ref(name_ref)?;
        match def.kind {
            DefKind::Function => {
                let sig = self.types.fn_sig_for_def(def)?.clone();
                Some(self.instantiate_call_sig(call, sig))
            }
            DefKind::Namespace => {
                let sigs = self.constructor_sigs_for_namespace(def);
                let sig = self.select_fn_sig(call, &sigs)?;
                Some(self.instantiate_call_sig(call, sig))
            }
            _ => None,
        }
    }

    fn resolve_static_member_defs(&self, field: &FieldExpr) -> Vec<Definition> {
        let Some(receiver) = field.receiver() else {
            return Vec::new();
        };
        let Expr::NameRef(namespace_ref) = receiver else {
            return Vec::new();
        };
        let Some(namespace_token) = namespace_ref.token() else {
            return Vec::new();
        };
        let namespace_span = self.span(namespace_token.text_range());
        let Some(namespace_def) = self
            .resolution
            .refs
            .iter()
            .find(|r| r.span == namespace_span)
            .and_then(|resolved| resolved.target.as_ref().ok())
        else {
            return Vec::new();
        };
        let Some(field_name) = field.field_name() else {
            return Vec::new();
        };
        resolve_namespace_members(&self.resolution.scopes, namespace_def, field_name.text())
    }

    fn resolve_static_member_def(&self, field: &FieldExpr) -> Option<Definition> {
        self.resolve_static_member_defs(field).into_iter().next()
    }

    fn resolve_method_member_def(&self, field: &FieldExpr, receiver_ty: &Ty) -> Option<Definition> {
        let offset = field.field_name()?.text_range().start();
        let field_name = field.field_name()?;
        let member_name = field_name.text();
        resolve_member_for_receiver(
            &self.resolution.scopes,
            &self.types.env,
            self.resolution.source,
            offset,
            receiver_ty,
            member_name,
        )
    }

    fn instantiate_method_sig(
        &self,
        receiver_ty: &Ty,
        def: &Definition,
        arg_tys: &[Ty],
    ) -> Option<BoundMethodSig> {
        let sig = self.types.fn_sig_for_def(def)?.clone();
        let (self_param, params) = sig.positional_params.split_first()?;
        if self_param.name != "self" || sig.ret.is_poison() {
            return None;
        }
        let mut substitutions = vec![None; sig.type_params.len()];
        bind_type_params(&self_param.ty, receiver_ty, &mut substitutions);

        let mut bound_params: Vec<ParamDef> = params
            .iter()
            .map(|param| ParamDef {
                name: param.name.clone(),
                ty: substitute_bound_type_params(&param.ty, &substitutions),
                doc: param.doc.clone(),
            })
            .collect();
        for (param, arg_ty) in bound_params.iter().zip(arg_tys.iter()) {
            infer_type_params(&param.ty, arg_ty, &mut substitutions);
        }
        bound_params = params
            .iter()
            .map(|param| ParamDef {
                name: param.name.clone(),
                ty: substitute_bound_type_params(&param.ty, &substitutions),
                doc: param.doc.clone(),
            })
            .collect();
        let ret = substitute_bound_type_params(&sig.ret, &substitutions);
        if ret.is_poison() || bound_params.iter().any(|param| param.ty.is_poison()) {
            return None;
        }
        Some(BoundMethodSig {
            params: bound_params,
            ret,
            capabilities: sig.capabilities,
        })
    }

    fn bound_method_sig_for_call(
        &mut self,
        receiver_ty: &Ty,
        def: &Definition,
        call: &CallExpr,
    ) -> Option<BoundMethodSig> {
        let receiver_bound = self.instantiate_method_sig(receiver_ty, def, &[])?;
        self.populate_method_callback_param_types(call, &receiver_bound.params);

        let arg_tys: Vec<Ty> = call
            .args()
            .filter_map(|arg| arg.expr())
            .map(|arg| self.infer_expr(&arg))
            .collect();

        self.instantiate_method_sig(receiver_ty, def, &arg_tys)
            .or(Some(receiver_bound))
    }

    fn bind_closure_params(
        &mut self,
        closure: &ClosureExpr,
        types: &[Ty],
        slot_doc: Option<&str>,
        slot_name: &str,
    ) {
        let inherited_doc = slot_doc.map(|doc| format!("`{slot_name}` callback slot.\n\n{doc}"));
        for (param, ty) in closure.parameters().zip(types.iter()) {
            if let Some(name_tok) = param.name() {
                self.closure_param_types
                    .insert(self.span(name_tok.text_range()), ty.clone());
                if let Some(doc) = inherited_doc.as_ref() {
                    self.closure_param_docs
                        .insert(self.span(name_tok.text_range()), doc.clone());
                }
            }
        }
    }

    fn check_call_args(&mut self, call: &CallExpr, sig: &FnSig) {
        for (arg, param) in call.args().zip(sig.positional_params.iter()) {
            if let Some(arg_expr) = arg.expr() {
                let arg_ty = self.infer_expr(&arg_expr);
                if !arg_ty.is_poison() && !param.ty.is_poison() {
                    if !types_compatible(&arg_ty, &param.ty) {
                        self.push_error(TypeError {
                            range: arg_expr.syntax().text_range(),
                            kind: TypeErrorKind::ArgTypeMismatch {
                                param_name: param.name.clone(),
                                expected: param.ty.clone(),
                                found: arg_ty,
                            },
                        });
                    }
                }
            }
        }
    }

    fn check_keyword_args(&mut self, call: &CallExpr, sig: &FnSig) {
        for kw in call.keyword_args() {
            let Some(name) = kw.name().map(|token| token.text().to_string()) else {
                continue;
            };
            let Some(arg_expr) = kw.expr() else {
                continue;
            };
            let Some(param) = sig.keyword_params.iter().find(|param| param.name == name) else {
                self.infer_expr(&arg_expr);
                continue;
            };
            let arg_ty = self.infer_expr(&arg_expr);
            if !arg_ty.is_poison() && !param.ty.is_poison() && !types_compatible(&arg_ty, &param.ty)
            {
                self.push_error(TypeError {
                    range: arg_expr.syntax().text_range(),
                    kind: TypeErrorKind::ArgTypeMismatch {
                        param_name: param.name.clone(),
                        expected: param.ty.clone(),
                        found: arg_ty,
                    },
                });
            }
        }
    }

    fn check_method_args(
        &mut self,
        call: &CallExpr,
        params: &[vx_types::ParamDef],
        _method_name: &str,
    ) {
        for (arg, param) in call.args().zip(params.iter()) {
            if let Some(arg_expr) = arg.expr() {
                let arg_ty = self.infer_expr(&arg_expr);
                if !arg_ty.is_poison() && !param.ty.is_poison() {
                    if !types_compatible(&arg_ty, &param.ty) {
                        self.push_error(TypeError {
                            range: arg_expr.syntax().text_range(),
                            kind: TypeErrorKind::ArgTypeMismatch {
                                param_name: param.name.clone(),
                                expected: param.ty.clone(),
                                found: arg_ty,
                            },
                        });
                    }
                }
            }
        }
    }

    fn infer_field(&mut self, field: &FieldExpr) -> Ty {
        if let Some(static_fn_ty) = self.infer_static_namespace_field(field) {
            return static_fn_ty;
        }

        let receiver_ty = field
            .receiver()
            .map(|r| self.infer_expr(&r))
            .unwrap_or(Ty::Poison);

        let Some(field_name_tok) = field.field_name() else {
            return Ty::Poison;
        };
        let field_name = field_name_tok.text();

        // Check namespace methods/fields exposed as callable members.
        if let Some(member_def) = self.resolve_method_member_def(field, &receiver_ty) {
            self.record_member_def(field, member_def.clone());
            if let Some(method_sig) = self.instantiate_method_sig(&receiver_ty, &member_def, &[]) {
                return Ty::Function(vx_types::FunctionTy {
                    params: method_sig
                        .params
                        .into_iter()
                        .map(|param| param.ty)
                        .collect(),
                    ret: Box::new(method_sig.ret),
                    capabilities: method_sig.capabilities,
                });
            }
        }

        // Check struct fields from type environment
        if let Ty::Struct(ref struct_ty) = receiver_ty {
            if let vx_types::TypeDef::Struct(ref s) = self.types.env[struct_ty.id] {
                if let Some(field_def) = s.fields.iter().find(|f| f.name == field_name) {
                    return field_def.ty.clone();
                }
            }
        }

        Ty::Poison
    }

    fn infer_partial_app(&mut self, partial: &PartialAppExpr) -> Ty {
        let Some(callee) = partial.callee() else {
            return Ty::Poison;
        };

        if let Some(sig) = self.sig_for_expr(&callee) {
            return self.function_ty_from_sig(&sig);
        }

        self.infer_expr(&callee)
    }

    fn infer_block(&mut self, block: &BlockExpr) -> Ty {
        let mut last_ty = Ty::Unit;
        for stmt in block.statements() {
            match &stmt {
                Stmt::Expr(expr) => {
                    last_ty = self.infer_expr(expr);
                }
                Stmt::Return(ret) => {
                    if let Some(expr) = ret.expr() {
                        self.infer_expr(&expr);
                    }
                    last_ty = Ty::Unit;
                }
                _ => {
                    self.check_stmt(&stmt);
                    last_ty = Ty::Unit;
                }
            }
        }
        last_ty
    }

    fn infer_if(&mut self, if_expr: &IfExpr) -> Ty {
        if let Some(cond) = if_expr.condition() {
            self.infer_expr(&cond);
        }
        let then_ty = if_expr
            .then_branch()
            .map(|e| self.infer_expr(&e))
            .unwrap_or(Ty::Poison);
        if let Some(else_br) = if_expr.else_branch() {
            self.infer_expr(&else_br);
        }
        then_ty
    }

    fn infer_closure(&mut self, closure: &ClosureExpr) -> Ty {
        let params: Vec<Ty> = closure
            .parameters()
            .map(|param| {
                param
                    .name()
                    .and_then(|name| {
                        self.closure_param_types
                            .get(&self.span(name.text_range()))
                            .cloned()
                    })
                    .unwrap_or(Ty::Poison)
            })
            .collect();
        let ret = closure
            .body()
            .map(|body| self.infer_expr(&body))
            .unwrap_or(Ty::Poison);
        Ty::Function(vx_types::FunctionTy {
            params,
            ret: Box::new(ret),
            capabilities: vec![],
        })
    }

    fn infer_where(&mut self, where_expr: &WhereExpr) -> Ty {
        let Some(callee) = where_expr.callee() else {
            return Ty::Poison;
        };

        let callee_ty = self.infer_expr(&callee);
        if let Some(keywords) = where_expr.keywords() {
            if let Some(sig) = self.where_sig(&callee) {
                if let Some(record) = self.record_for_keywords_expr(&keywords) {
                    self.populate_record_callback_param_types(&record, &sig);
                    self.check_record_keyword_args(&record, &sig);
                    let specialized_sig = self.specialize_sig_from_record(&sig, &record);
                    return match callee {
                        Expr::Call(_) => specialized_sig.ret,
                        _ => self.function_ty_from_sig(&specialized_sig),
                    };
                } else {
                    self.infer_expr(&keywords);
                }
            } else {
                self.infer_expr(&keywords);
            }
        }
        callee_ty
    }

    fn where_sig(&mut self, callee: &Expr) -> Option<FnSig> {
        match callee {
            Expr::Call(call) => {
                let root = call.root_callee()?;
                match root {
                    Expr::NameRef(name_ref) => self.call_sig_for_name_ref(call, &name_ref),
                    Expr::Field(field_expr) => {
                        self.infer_static_namespace_call_sig(call, &field_expr)
                    }
                    _ => None,
                }
            }
            _ => self.sig_for_expr(callee),
        }
    }

    fn infer_invoke(&mut self, invoke: &InvokeExpr) -> Ty {
        let Some(callee) = invoke.callee() else {
            return Ty::Poison;
        };

        match &callee {
            Expr::Field(field) => {
                let receiver_ty = field
                    .receiver()
                    .map(|expr| self.infer_expr(&expr))
                    .unwrap_or(Ty::Poison);
                if field.field_name().is_none() {
                    return Ty::Poison;
                }

                if let Some(member_def) = self.resolve_method_member_def(field, &receiver_ty)
                    && let Some(method_sig) =
                        self.instantiate_method_sig(&receiver_ty, &member_def, &[])
                    && method_sig.params.is_empty()
                {
                    return method_sig.ret;
                }

                Ty::Poison
            }
            _ => match self.infer_expr(&callee) {
                Ty::Function(fn_ty) if fn_ty.params.is_empty() => (*fn_ty.ret).clone(),
                _ => Ty::Poison,
            },
        }
    }

    fn infer_match(&mut self, m: &MatchExpr) -> Ty {
        // Infer the scrutinee (first child expr)
        let scrutinee_ty = m
            .syntax()
            .children()
            .find_map(Expr::cast)
            .map(|s| self.infer_expr(&s))
            .unwrap_or(Ty::Poison);

        // Infer each arm body; the match type is the first arm's body type
        let mut result_ty = Ty::Poison;
        for arm in m.arms() {
            // Type pattern bindings based on the scrutinee type
            if let Some(pat) = arm.pattern() {
                self.type_pattern(&pat, &scrutinee_ty);
            }

            if let Some(body) = arm.expr() {
                let arm_ty = self.infer_expr(&body);
                if result_ty.is_poison() && !arm_ty.is_poison() {
                    result_ty = arm_ty;
                }
            }
        }
        result_ty
    }

    /// Assign types to bindings introduced by a pattern, given the type of the value being matched.
    fn type_pattern(&mut self, pat: &Pattern, ty: &Ty) {
        match pat {
            Pattern::Binding(bp) => {
                if let Some(name_tok) = bp.name() {
                    if !ty.is_poison() {
                        self.binding_types
                            .insert(self.span(name_tok.text_range()), ty.clone());
                    }
                }
            }
            Pattern::Variant(vp) => {
                // Resolve the variant to find field types
                let variant_struct = self.resolve_variant_type(vp, ty);
                if let Some(struct_pat) = vp.fields() {
                    if let Some((struct_def_fields, _)) = &variant_struct {
                        for field in struct_pat.fields() {
                            if let Some(field_name_tok) = field.name() {
                                let field_name = field_name_tok.text();
                                let field_ty = struct_def_fields
                                    .iter()
                                    .find(|f| f.name == field_name)
                                    .map(|f| f.ty.clone())
                                    .unwrap_or(Ty::Poison);
                                // If there's a nested pattern (e.g., `Foo { bar: Baz { x } }`), recurse
                                if let Some(nested_pat) = field.pattern() {
                                    self.type_pattern(&nested_pat, &field_ty);
                                } else {
                                    // Simple field binding: `Foo { bar }` — bar gets the field type
                                    if !field_ty.is_poison() {
                                        self.binding_types.insert(
                                            self.span(field_name_tok.text_range()),
                                            field_ty,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Pattern::Struct(sp) => {
                // Direct struct pattern (not under a variant): fields get types from the matched struct
                if let Ty::Struct(struct_ty) = ty {
                    if let vx_types::TypeDef::Struct(ref sdef) = self.types.env[struct_ty.id] {
                        for field in sp.fields() {
                            if let Some(field_name_tok) = field.name() {
                                let field_name = field_name_tok.text();
                                let field_ty = sdef
                                    .fields
                                    .iter()
                                    .find(|f| f.name == field_name)
                                    .map(|f| f.ty.clone())
                                    .unwrap_or(Ty::Poison);
                                if let Some(nested_pat) = field.pattern() {
                                    self.type_pattern(&nested_pat, &field_ty);
                                } else if !field_ty.is_poison() {
                                    self.binding_types
                                        .insert(self.span(field_name_tok.text_range()), field_ty);
                                }
                            }
                        }
                    }
                }
            }
            Pattern::Tuple(tp) => {
                if let Ty::Tuple(tuple_ty) = ty {
                    for (item, elem_ty) in tp.items().zip(tuple_ty.elements.iter()) {
                        self.type_pattern(&item, elem_ty);
                    }
                }
            }
            Pattern::At(at) => {
                // `name @ pattern` — bind name to the whole type, recurse into pattern
                if let Some(name_tok) = at.binding_name() {
                    if !ty.is_poison() {
                        self.binding_types
                            .insert(self.span(name_tok.text_range()), ty.clone());
                    }
                }
                if let Some(inner) = at.pattern() {
                    self.type_pattern(&inner, ty);
                }
            }
            Pattern::Or(or) => {
                for alt in or.patterns() {
                    self.type_pattern(&alt, ty);
                }
            }
            Pattern::Paren(p) => {
                if let Some(inner) = p.inner() {
                    self.type_pattern(&inner, ty);
                }
            }
            Pattern::Array(arr) => {
                if let Ty::Array(elem_ty) = ty {
                    for item in arr.items() {
                        self.type_pattern(&item, elem_ty);
                    }
                }
            }
            _ => {}
        }
    }

    /// Resolve a variant pattern to find the struct definition for its fields.
    fn resolve_variant_type(
        &self,
        vp: &vx_parser::VariantPattern,
        scrutinee_ty: &Ty,
    ) -> Option<(Vec<vx_types::FieldDef>, vx_types::TypeId)> {
        let variant_name: String = vp.segments().last()?.text().to_string();

        // If the scrutinee is an enum, find the matching variant
        if let Ty::Enum(enum_ty) = scrutinee_ty {
            if let vx_types::TypeDef::Enum(ref edef) = self.types.env[enum_ty.id] {
                for &variant_id in &edef.variants {
                    if let vx_types::TypeDef::Struct(ref sdef) = self.types.env[variant_id] {
                        if sdef.name == variant_name {
                            return Some((sdef.fields.clone(), variant_id));
                        }
                    }
                }
            }
        }

        // Also try resolving by name if the scrutinee type is unknown
        let type_ids = self.types.env.lookup(&variant_name);
        for &id in type_ids {
            if let vx_types::TypeDef::Struct(ref sdef) = self.types.env[id] {
                if sdef.parent_enum.is_some() {
                    return Some((sdef.fields.clone(), id));
                }
            }
        }

        None
    }

    fn infer_binary(&mut self, bin: &BinaryExpr) -> Ty {
        if binary_operator_kind(bin) == Some(SyntaxKind::PIPE_GT) {
            return self.infer_pipe(bin);
        }

        let lhs_ty = bin.lhs().map(|e| self.infer_expr(&e)).unwrap_or(Ty::Poison);
        let rhs_ty = bin.rhs().map(|e| self.infer_expr(&e)).unwrap_or(Ty::Poison);

        match binary_operator_kind(bin) {
            // Comparison and equality operators always produce Bool
            Some(
                SyntaxKind::EQ2
                | SyntaxKind::BANG_EQ
                | SyntaxKind::LT
                | SyntaxKind::LT_EQ
                | SyntaxKind::GT
                | SyntaxKind::GT_EQ,
            ) => Ty::Scalar(vx_types::Scalar::Bool),
            // Logical operators produce Bool
            Some(SyntaxKind::AMP2 | SyntaxKind::PIPE2) => Ty::Scalar(vx_types::Scalar::Bool),
            // Arithmetic operators: result type matches the lhs (or rhs if lhs is Poison)
            Some(
                SyntaxKind::PLUS
                | SyntaxKind::MINUS
                | SyntaxKind::STAR
                | SyntaxKind::SLASH
                | SyntaxKind::PERCENT,
            ) => {
                if !lhs_ty.is_poison() {
                    lhs_ty
                } else {
                    rhs_ty
                }
            }
            _ => Ty::Poison,
        }
    }

    fn infer_pipe(&mut self, bin: &BinaryExpr) -> Ty {
        let Some(lhs) = bin.lhs() else {
            return Ty::Poison;
        };
        let Some(rhs) = bin.rhs() else {
            return Ty::Poison;
        };

        let lhs_ty = self.infer_expr(&lhs);
        let placeholders = collect_pipe_placeholder_spans(self.resolution.source, &rhs);
        if !placeholders.is_empty() {
            for span in &placeholders {
                self.pipe_placeholder_types.insert(*span, lhs_ty.clone());
            }
            let rhs_ty = self.infer_expr(&rhs);
            for span in placeholders {
                self.pipe_placeholder_types.remove(&span);
            }
            return rhs_ty;
        }

        match self.infer_expr(&rhs) {
            Ty::Function(function) if !function.params.is_empty() => {
                let first_param = &function.params[0];
                if !lhs_ty.is_poison()
                    && !first_param.is_poison()
                    && !types_compatible(&lhs_ty, first_param)
                {
                    self.push_error(TypeError {
                        range: lhs.syntax().text_range(),
                        kind: TypeErrorKind::ArgTypeMismatch {
                            param_name: "_".to_string(),
                            expected: first_param.clone(),
                            found: lhs_ty,
                        },
                    });
                }
                (*function.ret).clone()
            }
            _ => Ty::Poison,
        }
    }

    fn infer_prefix(&mut self, pre: &PrefixExpr) -> Ty {
        let operand_ty = pre
            .expr()
            .map(|e| self.infer_expr(&e))
            .unwrap_or(Ty::Poison);

        match pre.operator_token().map(|t| t.kind()) {
            Some(SyntaxKind::BANG) => Ty::Scalar(vx_types::Scalar::Bool),
            Some(SyntaxKind::MINUS) => operand_ty,
            _ => Ty::Poison,
        }
    }

    fn infer_fallible(&mut self, fallible: &FallibleExpr) -> Ty {
        let Some(expr) = fallible.expr() else {
            return Ty::Poison;
        };
        let inner_ty = self.infer_expr(&expr);
        if inner_ty.is_poison() {
            return Ty::Poison;
        }

        match inner_ty {
            Ty::Function(fn_ty) => Ty::Function(vx_types::FunctionTy {
                params: fn_ty.params,
                ret: Box::new(Ty::Optional(Box::new((*fn_ty.ret).clone()))),
                capabilities: fn_ty.capabilities,
            }),
            other => Ty::Optional(Box::new(other)),
        }
    }

    fn infer_with(&mut self, w: &WithExpr) -> Ty {
        // The with-expression preserves the type of its receiver:
        // `foo with { field: value }` has the same type as `foo`
        let receiver_ty = w
            .receiver()
            .map(|r| self.infer_expr(&r))
            .unwrap_or(Ty::Poison);

        // Infer types of update field values (inside the update body)
        for child in w.syntax().children() {
            if child.kind() == SyntaxKind::UPDATE_BODY {
                for field_node in child.children() {
                    for expr_child in field_node.children() {
                        if let Some(e) = Expr::cast(expr_child) {
                            self.infer_expr(&e);
                        }
                    }
                }
            }
        }

        receiver_ty
    }

    fn infer_map(&mut self, map: &MapExpr) -> Ty {
        let mut key_ty = None;
        let mut val_ty = None;
        for entry in map.entries() {
            if let Some(k) = entry.key() {
                let ty = self.infer_expr(&k);
                if !ty.is_poison() && key_ty.is_none() {
                    key_ty = Some(ty);
                }
            }
            if let Some(v) = entry.value() {
                let ty = self.infer_expr(&v);
                if !ty.is_poison() && val_ty.is_none() {
                    val_ty = Some(ty);
                }
            }
        }
        Ty::Map(
            Box::new(key_ty.unwrap_or(Ty::Poison)),
            Box::new(val_ty.unwrap_or(Ty::Poison)),
        )
    }

    fn infer_record(&mut self, record: &RecordExpr) -> Ty {
        for field in record.fields() {
            if let Some(spread) = field.spread() {
                if let Some(expr) = spread.expr() {
                    self.infer_expr(&expr);
                }
            } else if let Some(expr) = field.value() {
                let expr = Self::peel_parens(expr);
                if matches!(expr, Expr::Closure(_)) {
                    continue;
                }
                self.infer_expr(&expr);
            }
        }
        Ty::Poison
    }

    fn record_for_keywords_expr(&self, expr: &Expr) -> Option<RecordExpr> {
        match Self::peel_parens(expr.clone()) {
            Expr::Record(record) => Some(record),
            Expr::NameRef(name_ref) => {
                let def = self.resolved_def_for_name_ref(&name_ref)?;
                self.binding_initializer_expr(def)
                    .and_then(|init| self.record_for_keywords_expr(&init))
            }
            _ => None,
        }
    }

    fn binding_initializer_expr(&self, def: &Definition) -> Option<Expr> {
        if def.kind != DefKind::Binding || def.source() != self.resolution.source {
            return None;
        }

        self.file
            .syntax()
            .descendants()
            .filter_map(LetStmt::cast)
            .find_map(|let_stmt| {
                let name = let_stmt.name()?;
                (self.span(name.text_range()) == def.name_span)
                    .then(|| let_stmt.expr())
                    .flatten()
            })
    }

    fn infer_struct_literal(&mut self, sl: &StructLiteralExpr) -> Ty {
        // Infer field value expressions
        for field in sl.fields() {
            if let Some(expr) = field.expr() {
                self.infer_expr(&expr);
            }
        }
        // Resolve the struct name to a TypeId
        let Some(name_tok) = sl.name() else {
            return Ty::Poison;
        };
        let name = name_tok.text();
        let type_ids = self.types.env.lookup(name);
        for &id in type_ids {
            if let vx_types::TypeDef::Struct(ref sdef) = self.types.env[id] {
                if sdef.parent_enum.is_none() {
                    return Ty::Struct(vx_types::StructTy {
                        id,
                        type_args: vec![],
                    });
                }
            }
        }
        Ty::Poison
    }

    fn infer_array(&mut self, arr: &ArrayExpr) -> Ty {
        let mut element_ty = None;
        for child in arr.syntax().children() {
            if let Some(e) = Expr::cast(child) {
                let ty = self.infer_expr(&e);
                if !ty.is_poison() && element_ty.is_none() {
                    element_ty = Some(ty);
                }
            }
        }
        Ty::Array(Box::new(element_ty.unwrap_or(Ty::Poison)))
    }

    fn infer_static_namespace_field(&mut self, field: &FieldExpr) -> Option<Ty> {
        if let Some(member_def) = self.resolve_static_member_def(field) {
            self.record_member_def(field, member_def.clone());
            if let Some(sig) = self.types.fn_sig_for_def(&member_def) {
                return Some(Ty::Function(vx_types::FunctionTy {
                    params: sig
                        .positional_params
                        .iter()
                        .map(|param| param.ty.clone())
                        .collect(),
                    ret: Box::new(sig.ret.clone()),
                    capabilities: sig.capabilities.clone(),
                }));
            }
        }
        None
    }
}

/// Check if two types are compatible (for argument checking).
/// This is a simple structural check, not full unification.
fn types_compatible(actual: &Ty, expected: &Ty) -> bool {
    if actual.is_poison() || expected.is_poison() {
        return true;
    }
    if actual == expected {
        return true;
    }
    match (actual, expected) {
        (Ty::Array(_), Ty::Array(_)) => true, // element type checking later
        (Ty::Map(_, _), Ty::Map(_, _)) => true,
        (Ty::Stream(_), Ty::Stream(_)) => true,
        (Ty::Function(_), Ty::Function(_)) => true,
        // For now, be permissive with other combinations
        _ => {
            // If both are concrete but different shapes, it's an error
            // But we're lenient with unknowns
            matches!(
                (actual, expected),
                (Ty::Infer(_), _) | (_, Ty::Infer(_)) | (Ty::Param(_), _) | (_, Ty::Param(_))
            )
        }
    }
}

fn binary_operator_kind(binary: &BinaryExpr) -> Option<SyntaxKind> {
    binary.syntax().children_with_tokens().find_map(|child| {
        let rowan::NodeOrToken::Token(token) = child else {
            return None;
        };
        match token.kind() {
            SyntaxKind::PIPE_GT
            | SyntaxKind::PLUS
            | SyntaxKind::MINUS
            | SyntaxKind::STAR
            | SyntaxKind::SLASH
            | SyntaxKind::PERCENT
            | SyntaxKind::AMP2
            | SyntaxKind::PIPE2
            | SyntaxKind::EQ2
            | SyntaxKind::BANG_EQ
            | SyntaxKind::LT
            | SyntaxKind::LT_EQ
            | SyntaxKind::GT
            | SyntaxKind::GT_EQ => Some(token.kind()),
            _ => None,
        }
    })
}

fn collect_pipe_placeholder_spans(source: crate::SourceId, expr: &Expr) -> Vec<SourceSpan> {
    expr.syntax()
        .descendants()
        .filter_map(NameRefExpr::cast)
        .filter_map(|name_ref| {
            let token = name_ref.token()?;
            (token.kind() == SyntaxKind::UNDERSCORE)
                .then(|| SourceSpan::new(source, token.text_range()))
        })
        .collect()
}

fn call_matches(arg_tys: &[Ty], sig: &FnSig) -> bool {
    arg_tys.len() == sig.positional_params.len()
        && arg_tys
            .iter()
            .zip(sig.positional_params.iter())
            .all(|(arg_ty, param)| types_compatible(arg_ty, &param.ty))
}

fn substitute_type_params(ty: &Ty, substitutions: &[Ty]) -> Ty {
    match ty {
        Ty::Param(param) => substitutions
            .get(param.index as usize)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Ty::Optional(inner) => Ty::Optional(Box::new(substitute_type_params(inner, substitutions))),
        Ty::Array(inner) => Ty::Array(Box::new(substitute_type_params(inner, substitutions))),
        Ty::Map(key, value) => Ty::Map(
            Box::new(substitute_type_params(key, substitutions)),
            Box::new(substitute_type_params(value, substitutions)),
        ),
        Ty::Stream(inner) => Ty::Stream(Box::new(substitute_type_params(inner, substitutions))),
        Ty::Tuple(tuple) => Ty::Tuple(vx_types::TupleTy {
            elements: tuple
                .elements
                .iter()
                .map(|element| substitute_type_params(element, substitutions))
                .collect(),
        }),
        Ty::Function(function) => Ty::Function(vx_types::FunctionTy {
            params: function
                .params
                .iter()
                .map(|param| substitute_type_params(param, substitutions))
                .collect(),
            ret: Box::new(substitute_type_params(&function.ret, substitutions)),
            capabilities: function.capabilities.clone(),
        }),
        _ => ty.clone(),
    }
}

fn substitute_bound_type_params(ty: &Ty, substitutions: &[Option<Ty>]) -> Ty {
    match ty {
        Ty::Param(param) => substitutions
            .get(param.index as usize)
            .and_then(|ty| ty.clone())
            .unwrap_or_else(|| ty.clone()),
        Ty::Optional(inner) => {
            Ty::Optional(Box::new(substitute_bound_type_params(inner, substitutions)))
        }
        Ty::Array(inner) => Ty::Array(Box::new(substitute_bound_type_params(inner, substitutions))),
        Ty::Map(key, value) => Ty::Map(
            Box::new(substitute_bound_type_params(key, substitutions)),
            Box::new(substitute_bound_type_params(value, substitutions)),
        ),
        Ty::Stream(inner) => {
            Ty::Stream(Box::new(substitute_bound_type_params(inner, substitutions)))
        }
        Ty::Tuple(tuple) => Ty::Tuple(vx_types::TupleTy {
            elements: tuple
                .elements
                .iter()
                .map(|element| substitute_bound_type_params(element, substitutions))
                .collect(),
        }),
        Ty::Function(function) => Ty::Function(vx_types::FunctionTy {
            params: function
                .params
                .iter()
                .map(|param| substitute_bound_type_params(param, substitutions))
                .collect(),
            ret: Box::new(substitute_bound_type_params(&function.ret, substitutions)),
            capabilities: function.capabilities.clone(),
        }),
        _ => ty.clone(),
    }
}

fn bind_type_params(expected: &Ty, actual: &Ty, substitutions: &mut [Option<Ty>]) {
    match expected {
        Ty::Param(param) => {
            let slot = &mut substitutions[param.index as usize];
            if slot.is_none() {
                *slot = Some(actual.clone());
            }
        }
        Ty::Optional(inner) => {
            if let Ty::Optional(actual_inner) = actual {
                bind_type_params(inner, actual_inner, substitutions);
            }
        }
        Ty::Array(inner) => {
            if let Ty::Array(actual_inner) = actual {
                bind_type_params(inner, actual_inner, substitutions);
            }
        }
        Ty::Map(expected_key, expected_value) => {
            if let Ty::Map(actual_key, actual_value) = actual {
                bind_type_params(expected_key, actual_key, substitutions);
                bind_type_params(expected_value, actual_value, substitutions);
            }
        }
        Ty::Stream(inner) => {
            if let Ty::Stream(actual_inner) = actual {
                bind_type_params(inner, actual_inner, substitutions);
            }
        }
        Ty::Tuple(expected_tuple) => {
            if let Ty::Tuple(actual_tuple) = actual {
                for (expected, actual) in expected_tuple.elements.iter().zip(&actual_tuple.elements)
                {
                    bind_type_params(expected, actual, substitutions);
                }
            }
        }
        Ty::Function(expected_function) => {
            if let Ty::Function(actual_function) = actual {
                for (expected, actual) in
                    expected_function.params.iter().zip(&actual_function.params)
                {
                    bind_type_params(expected, actual, substitutions);
                }
                bind_type_params(&expected_function.ret, &actual_function.ret, substitutions);
            }
        }
        _ => {}
    }
}

fn infer_type_params(expected: &Ty, actual: &Ty, substitutions: &mut [Option<Ty>]) {
    bind_type_params(expected, actual, substitutions);
}
