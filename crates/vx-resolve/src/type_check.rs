//! Expression typing pass for Vixen source files.
//!
//! A narrow, forward-only pass that infers types for expressions in the
//! current file. Not a full Hindley-Milner engine — just enough to provide
//! hover info and catch obvious type errors in builtin calls.

use std::collections::HashMap;

use rowan::TextRange;
use vx_parser::{
    AstNode, BlockExpr, CallExpr, ClosureExpr, Expr, FieldExpr, IfExpr, LiteralExpr, NameRefExpr,
    SourceFile, Stmt, SyntaxKind,
};
use vx_types::{Builtin, FnSig, Ty};

use crate::builtins::{builtin_fn_sig, builtin_method};
use crate::resolve::NameResolution;
use crate::scope::{DefKind, Definition};
use crate::type_collect::TypeCollection;

/// A type error found during expression typing.
#[derive(Debug, Clone)]
pub struct TypeError {
    pub range: TextRange,
    pub kind: TypeErrorKind,
}

/// The kind of type error.
#[derive(Debug, Clone)]
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
    pub expr_types: HashMap<TextRange, Ty>,
    /// Type errors found during checking.
    pub errors: Vec<TypeError>,
}

/// Run expression typing on a parsed source file.
pub fn type_check(
    file: &SourceFile,
    resolution: &NameResolution,
    types: &TypeCollection,
) -> TypeCheckResult {
    let mut checker = ExprTyper {
        resolution,
        types,
        expr_types: HashMap::new(),
        errors: Vec::new(),
        closure_param_types: HashMap::new(),
    };

    for item in file.items() {
        if let vx_parser::Item::FnDecl(fn_decl) = item {
            if let Some(body) = fn_decl.body() {
                checker.check_block(&body);
            }
        }
    }

    TypeCheckResult {
        expr_types: checker.expr_types,
        errors: checker.errors,
    }
}

struct ExprTyper<'a> {
    resolution: &'a NameResolution,
    types: &'a TypeCollection,
    expr_types: HashMap<TextRange, Ty>,
    errors: Vec<TypeError>,
    /// Types for closure parameters, populated when we see a closure in a
    /// known callback slot (e.g., exec observer's proc and tree params).
    closure_param_types: HashMap<TextRange, Ty>,
}

impl<'a> ExprTyper<'a> {
    fn check_block(&mut self, block: &BlockExpr) {
        for stmt in block.statements() {
            self.check_stmt(&stmt);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(let_stmt) => {
                if let Some(expr) = let_stmt.expr() {
                    self.infer_expr(&expr);
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
            Expr::NameRef(name_ref) => self.infer_name_ref(name_ref),
            Expr::Call(call) => self.infer_call(call),
            Expr::Field(field) => self.infer_field(field),
            Expr::Block(block) => self.infer_block(block),
            Expr::If(if_expr) => self.infer_if(if_expr),
            Expr::Closure(closure) => self.infer_closure(closure),
            Expr::Array(arr) => {
                // Infer element types but return Array[?]
                for child in arr.syntax().children() {
                    if let Some(e) = Expr::cast(child) {
                        self.infer_expr(&e);
                    }
                }
                Ty::Array(Box::new(Ty::Poison))
            }
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
                .insert(expr.syntax().text_range(), ty.clone());
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

    fn infer_name_ref(&self, name_ref: &NameRefExpr) -> Ty {
        let Some(token) = name_ref.token() else {
            return Ty::Poison;
        };
        let ref_range = token.text_range();

        // Look up in name resolution
        let resolved = self
            .resolution
            .refs
            .iter()
            .find(|r| r.ref_range == ref_range);

        if let Some(resolved) = resolved {
            if let Ok(def) = &resolved.target {
                // Check if this definition is a closure parameter with a known type
                if let Some(ty) = self.closure_param_types.get(&def.name_range) {
                    return ty.clone();
                }
                return self.type_of_def(def);
            }
        }

        // Not resolved — might be a builtin function name
        let name = token.text();
        if builtin_fn_sig(name).is_some() {
            return Ty::Poison;
        }
        Ty::Poison
    }

    fn type_of_def(&self, def: &Definition) -> Ty {
        match def.kind {
            DefKind::Parameter => {
                // Check if this parameter has a type from closure_param_types
                if let Some(ty) = self.closure_param_types.get(&def.name_range) {
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
                if let Some(sig) = self.types.range_to_fn_sig.get(&def.name_range) {
                    return sig.ret.clone();
                }
                Ty::Poison
            }
            DefKind::Binding => {
                // For let bindings, we'd need to look at the initializer type.
                // This is tracked in expr_types if the init was already typed.
                Ty::Poison
            }
            _ => Ty::Poison,
        }
    }

    fn find_fn_sig_for_param(&self, _def: &Definition) -> Option<Ty> {
        // Parameter types from declared functions are in the FnSig.
        // We'd need to match the parameter definition to its enclosing function,
        // then look up the FnSig. For now, we rely on closure_param_types for
        // callback params and FnSig for declared function params.
        None
    }

    fn infer_call(&mut self, call: &CallExpr) -> Ty {
        let Some(callee) = call.callee() else {
            return Ty::Poison;
        };

        match &callee {
            Expr::NameRef(name_ref) => {
                let callee_name = name_ref.token().map(|t| t.text().to_string());
                let Some(callee_name) = callee_name else {
                    return Ty::Poison;
                };

                // Populate closure param types for known callback slots
                self.populate_callback_param_types(call, &callee_name);

                // Check for builtin function
                if let Some(sig) = builtin_fn_sig(&callee_name) {
                    // Type-check positional arguments
                    self.check_call_args(call, &sig);
                    // Infer all argument expressions
                    self.infer_call_arg_exprs(call);
                    return sig.ret.clone();
                }

                // Check for user-declared function
                let ref_range = name_ref.token().map(|t| t.text_range());
                if let Some(ref_range) = ref_range {
                    let resolved = self
                        .resolution
                        .refs
                        .iter()
                        .find(|r| r.ref_range == ref_range);
                    if let Some(resolved) = resolved
                        && let Ok(def) = &resolved.target
                        && def.kind == DefKind::Function
                    {
                        if let Some(sig) = self.types.range_to_fn_sig.get(&def.name_range) {
                            self.infer_call_arg_exprs(call);
                            return sig.ret.clone();
                        }
                    }
                }

                self.infer_call_arg_exprs(call);
                Ty::Poison
            }
            Expr::Field(field_expr) => {
                // Method call: receiver.method(args)
                let receiver_ty = field_expr
                    .receiver()
                    .map(|r| self.infer_expr(&r))
                    .unwrap_or(Ty::Poison);
                let method_name = field_expr
                    .field_name()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();

                if let Ty::Builtin(ref builtin) = receiver_ty {
                    if let Some(method_sig) = builtin_method(builtin, &method_name) {
                        // Type-check method arguments
                        self.check_method_args(call, &method_sig.params, &method_name);
                        self.infer_call_arg_exprs(call);
                        return method_sig.ret.clone();
                    }

                    self.errors.push(TypeError {
                        range: field_expr.syntax().text_range(),
                        kind: TypeErrorKind::NoSuchMethod {
                            receiver_ty: receiver_ty.clone(),
                            method_name,
                        },
                    });
                }

                self.infer_call_arg_exprs(call);
                Ty::Poison
            }
            _ => {
                self.infer_expr(&callee);
                self.infer_call_arg_exprs(call);
                Ty::Poison
            }
        }
    }

    fn infer_call_arg_exprs(&mut self, call: &CallExpr) {
        for arg in call.args() {
            if let Some(arg_expr) = arg.expr() {
                self.infer_expr(&arg_expr);
            }
        }
        for kw_arg in call.keyword_args() {
            if let Some(arg_expr) = kw_arg.expr() {
                self.infer_expr(&arg_expr);
            }
        }
    }

    /// Populate closure parameter types for known callback slots.
    fn populate_callback_param_types(&mut self, call: &CallExpr, callee_name: &str) {
        match callee_name {
            "build" => {
                // build(entry: (path, args, env) => ...)
                let first_arg = call.args().next();
                let first_expr = first_arg.as_ref().and_then(|a| a.expr());
                if let Some(Expr::Closure(closure)) = first_expr.as_ref() {
                    let param_types = &[
                        Ty::Builtin(Builtin::Path),
                        Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))),
                        Ty::Map(
                            Box::new(Ty::Scalar(vx_types::Scalar::String)),
                            Box::new(Ty::Scalar(vx_types::Scalar::String)),
                        ),
                    ];
                    self.bind_closure_params(closure, param_types);
                }
            }
            "exec" => {
                for kw in call.keyword_args() {
                    let Some(name) = kw.name() else { continue };
                    let kw_expr = kw.expr();
                    let Some(Expr::Closure(closure)) = kw_expr.as_ref() else {
                        continue;
                    };
                    match name.text() {
                        "observer" => {
                            self.bind_closure_params(
                                closure,
                                &[Ty::Builtin(Builtin::Process), Ty::Builtin(Builtin::Tree)],
                            );
                        }
                        "verifier" => {
                            self.bind_closure_params(closure, &[Ty::Builtin(Builtin::Host)]);
                        }
                        _ => {}
                    }
                }
            }
            "stabilize" => {
                let args: Vec<_> = call.args().collect();
                // Second positional arg: resolve callback gets witness result
                if let Some(resolve_arg) = args.get(1) {
                    let resolve_expr = resolve_arg.expr();
                    if let Some(Expr::Closure(closure)) = resolve_expr.as_ref() {
                        // The resolve callback receives the witness result
                        // We don't know the witness type, so leave as Poison
                        self.bind_closure_params(closure, &[Ty::Poison]);
                    }
                }
            }
            _ => {}
        }
    }

    fn bind_closure_params(&mut self, closure: &ClosureExpr, types: &[Ty]) {
        for (param, ty) in closure.parameters().zip(types.iter()) {
            if let Some(name_tok) = param.name() {
                self.closure_param_types
                    .insert(name_tok.text_range(), ty.clone());
            }
        }
    }

    fn check_call_args(&mut self, call: &CallExpr, sig: &FnSig) {
        for (arg, param) in call.args().zip(sig.positional_params.iter()) {
            if let Some(arg_expr) = arg.expr() {
                let arg_ty = self.infer_expr(&arg_expr);
                if !arg_ty.is_poison() && !param.ty.is_poison() {
                    if !types_compatible(&arg_ty, &param.ty) {
                        self.errors.push(TypeError {
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
                        self.errors.push(TypeError {
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
        let receiver_ty = field
            .receiver()
            .map(|r| self.infer_expr(&r))
            .unwrap_or(Ty::Poison);

        let Some(field_name_tok) = field.field_name() else {
            return Ty::Poison;
        };
        let field_name = field_name_tok.text();

        // Check builtin type methods/fields (property access without call)
        if let Ty::Builtin(ref builtin) = receiver_ty {
            if let Some(method_sig) = builtin_method(builtin, field_name) {
                if method_sig.params.is_empty() {
                    // Property-style access (no args needed)
                    return method_sig.ret.clone();
                }
                // It's a method — return a function type (will be called)
                return Ty::Poison;
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
        if let Some(body) = closure.body() {
            self.infer_expr(&body);
        }
        Ty::Poison
    }
}

/// Check if two types are compatible (for argument checking).
/// This is a simple structural check, not full unification.
fn types_compatible(actual: &Ty, expected: &Ty) -> bool {
    if actual.is_poison() || expected.is_poison() {
        return true;
    }
    match (actual, expected) {
        (Ty::Scalar(a), Ty::Scalar(b)) => a == b,
        (Ty::Unit, Ty::Unit) => true,
        (Ty::Builtin(a), Ty::Builtin(b)) => a == b,
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
