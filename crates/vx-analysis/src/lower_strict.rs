//! Strict-fragment lowering: walks AST expressions within a KernelRegion and
//! produces a `VixenTypedFunction` ready for Kajit compilation.
//!
//! This replaces the `vx-jit` analysis_adapter + kajit_backend pipeline with a
//! single pass driven by the analysis result rather than an independent AST re-walk.

use std::collections::HashMap;

use kajit_hir::{
    BinaryOp, FieldDef, Literal, LocalId, Module, Type, TypeDef, TypeDefId, TypeDefKind,
    VixenCallableRef, VixenTypedExpr, VixenTypedFunction, VixenTypedLocal, VixenTypedParam,
    VixenTypedStmt,
};
use vx_parser::{AstNode, Expr, FnDecl, Stmt, SyntaxKind};
use vx_resolve::SourceSpan;
use vx_types::{Scalar, Ty};

use crate::{BodyId, ExprId, KernelRegion, SingleSourceBridge};

/// Errors that indicate a kernel cannot be strict-lowered.
#[derive(Debug, Clone)]
pub enum StrictLoweringError {
    UnsupportedType(String),
    UnsupportedExpr(String),
    UnsupportedBinaryOp(String),
    UnsupportedPattern(String),
    UnsupportedRange(String),
    MissingAst(BodyId),
    MissingExprType(String),
}

impl std::fmt::Display for StrictLoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedType(s) => write!(f, "unsupported type: {s}"),
            Self::UnsupportedExpr(s) => write!(f, "unsupported expression: {s}"),
            Self::UnsupportedBinaryOp(s) => write!(f, "unsupported binary op: {s}"),
            Self::UnsupportedPattern(s) => write!(f, "unsupported pattern: {s}"),
            Self::UnsupportedRange(s) => write!(f, "unsupported range: {s}"),
            Self::MissingAst(id) => write!(f, "missing AST for body {}", id.0),
            Self::MissingExprType(s) => write!(f, "missing type for expr: {s}"),
        }
    }
}

/// The strict lowering context, holding the Kajit module with Str type registered.
pub struct StrictLowerer {
    pub module: Module,
    str_def: TypeDefId,
}

#[derive(Debug, Clone, Copy)]
struct BoundLocal {
    local: LocalId,
    ty: StrictTy,
}

/// The subset of types that strict fragments support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrictTy {
    U64,
    Bool,
    String,
}

struct LoweringState {
    /// Locals from let-bindings in the function body.
    body_locals: Vec<VixenTypedLocal>,
    /// Synthetic locals created for concat temporaries etc.
    synthetic_locals: Vec<VixenTypedLocal>,
    next_local_id: u32,
}

impl StrictLowerer {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let mut module = Module::new();
        let _ = module.install_runtime_memory_callables();
        let str_def = module.add_type_def(TypeDef {
            name: "Str".to_owned(),
            generic_params: Vec::new(),
            kind: TypeDefKind::Struct {
                fields: vec![
                    FieldDef {
                        name: "ptr".to_owned(),
                        ty: Type::u(64),
                        offset: None,
                    },
                    FieldDef {
                        name: "len".to_owned(),
                        ty: Type::u(64),
                        offset: None,
                    },
                ],
            },
            size: None,
            transparent: false,
        });
        Self { module, str_def }
    }

    fn str_type(&self) -> Type {
        Type::named(self.str_def, Vec::new())
    }

    fn to_kajit_type(&self, ty: StrictTy) -> Type {
        match ty {
            StrictTy::U64 => Type::u(64),
            StrictTy::Bool => Type::bool(),
            StrictTy::String => self.str_type(),
        }
    }

    /// Try to lower a kernel region into a `VixenTypedFunction`.
    /// Returns `Err` if the kernel contains anything Kajit can't compile.
    pub fn try_lower_kernel(
        &self,
        kernel: &KernelRegion,
        bridge: &SingleSourceBridge,
    ) -> Result<VixenTypedFunction, StrictLoweringError> {
        let fn_decl = find_fn_decl(kernel.body, bridge)
            .ok_or(StrictLoweringError::MissingAst(kernel.body))?;
        let body = bridge
            .bodies
            .iter()
            .find(|b| b.id == kernel.body)
            .ok_or(StrictLoweringError::MissingAst(kernel.body))?;
        let expr_ids = expr_ids_by_span(bridge);

        let mut locals_by_name = HashMap::new();
        let mut next_local = 0u32;

        // Lower params
        let mut params = Vec::new();
        for param in fn_decl.parameters() {
            let name = param
                .name()
                .map(|t| t.text().to_string())
                .ok_or_else(|| StrictLoweringError::UnsupportedType("missing param name".into()))?;
            let ty = param
                .slot_type()
                .and_then(|slot| slot.ty())
                .and_then(|t| strict_ty_from_parser_type(&t))
                .ok_or_else(|| StrictLoweringError::UnsupportedType(format!("param `{name}`")))?;
            let local = LocalId::new(next_local);
            next_local += 1;
            params.push(VixenTypedParam {
                local,
                name: name.clone(),
                ty: self.to_kajit_type(ty),
            });
            locals_by_name.insert(name, BoundLocal { local, ty });
        }

        // Return type
        let return_ty = fn_decl
            .return_type()
            .and_then(|ret| ret.ty())
            .and_then(|t| strict_ty_from_parser_type(&t))
            .ok_or_else(|| StrictLoweringError::UnsupportedType("return type".into()))?;

        let mut state = LoweringState {
            body_locals: Vec::new(),
            synthetic_locals: Vec::new(),
            next_local_id: next_local,
        };

        // Lower body
        let stmts =
            self.lower_function_body(&fn_decl, bridge, &expr_ids, &mut locals_by_name, &mut state)?;

        let mut locals = state.body_locals;
        locals.extend(state.synthetic_locals);

        Ok(VixenTypedFunction {
            name: body.name.clone(),
            params,
            locals,
            return_type: self.to_kajit_type(return_ty),
            body: stmts,
            comment: Some("lowered from vx-analysis strict kernel".to_owned()),
        })
    }

    fn lower_function_body(
        &self,
        fn_decl: &FnDecl,
        bridge: &SingleSourceBridge,
        expr_ids: &HashMap<SourceSpan, ExprId>,
        locals_by_name: &mut HashMap<String, BoundLocal>,
        state: &mut LoweringState,
    ) -> Result<Vec<VixenTypedStmt>, StrictLoweringError> {
        if let Some(block) = fn_decl.body() {
            return self.lower_block_stmts(&block, bridge, expr_ids, locals_by_name, state);
        }
        let body_expr = fn_decl
            .body_expr()
            .ok_or_else(|| StrictLoweringError::UnsupportedExpr("missing body".into()))?;
        let body_expr = peel_expr_wrappers(body_expr);
        self.lower_return_expr(&body_expr, bridge, expr_ids, locals_by_name, state)
    }

    fn lower_block_stmts(
        &self,
        block: &vx_parser::BlockExpr,
        bridge: &SingleSourceBridge,
        expr_ids: &HashMap<SourceSpan, ExprId>,
        locals_by_name: &mut HashMap<String, BoundLocal>,
        state: &mut LoweringState,
    ) -> Result<Vec<VixenTypedStmt>, StrictLoweringError> {
        let statements: Vec<_> = block
            .syntax()
            .children()
            .filter_map(vx_parser::Stmt::cast)
            .collect();
        let mut out = Vec::new();
        for (index, stmt) in statements.iter().enumerate() {
            let is_last = index + 1 == statements.len();
            match stmt {
                Stmt::Let(let_stmt) => {
                    out.push(self.lower_let_stmt(
                        let_stmt,
                        bridge,
                        expr_ids,
                        locals_by_name,
                        state,
                    )?);
                }
                Stmt::Return(ret) => {
                    let expr = ret.expr().ok_or_else(|| {
                        StrictLoweringError::UnsupportedExpr("return without expr".into())
                    })?;
                    let expr = peel_expr_wrappers(expr);
                    out.extend(self.lower_return_expr(
                        &expr,
                        bridge,
                        expr_ids,
                        locals_by_name,
                        state,
                    )?);
                }
                Stmt::Expr(expr) if is_last => {
                    let expr = peel_expr_wrappers(expr.clone());
                    out.extend(self.lower_return_expr(
                        &expr,
                        bridge,
                        expr_ids,
                        locals_by_name,
                        state,
                    )?);
                }
                _ => {
                    return Err(StrictLoweringError::UnsupportedExpr(format!(
                        "unsupported statement: {stmt:?}"
                    )));
                }
            }
        }
        Ok(out)
    }

    fn lower_let_stmt(
        &self,
        let_stmt: &vx_parser::LetStmt,
        bridge: &SingleSourceBridge,
        expr_ids: &HashMap<SourceSpan, ExprId>,
        locals_by_name: &mut HashMap<String, BoundLocal>,
        state: &mut LoweringState,
    ) -> Result<VixenTypedStmt, StrictLoweringError> {
        let Some(vx_parser::Pattern::Binding(pattern)) = let_stmt.pattern() else {
            return Err(StrictLoweringError::UnsupportedPattern(
                "non-binding".into(),
            ));
        };
        let name = pattern
            .name()
            .map(|t| t.text().to_string())
            .ok_or_else(|| StrictLoweringError::UnsupportedPattern("missing name".into()))?;
        let expr = let_stmt
            .expr()
            .ok_or_else(|| StrictLoweringError::UnsupportedExpr("let without expr".into()))?;
        let expr = peel_expr_wrappers(expr);

        // Resolve type from annotation, then from bridge expr_types
        let ty = let_stmt
            .slot_type()
            .and_then(|slot| slot.ty())
            .and_then(|t| strict_ty_from_parser_type(&t))
            .or_else(|| strict_ty_for_expr(&expr, bridge, expr_ids, locals_by_name))
            .ok_or_else(|| {
                StrictLoweringError::UnsupportedType(expr.syntax().text().to_string())
            })?;

        let local = LocalId::new(state.next_local_id);
        state.next_local_id += 1;
        let value = self.lower_expr(&expr, bridge, expr_ids, locals_by_name)?;
        state.body_locals.push(VixenTypedLocal {
            local,
            name: name.clone(),
            ty: self.to_kajit_type(ty),
        });
        locals_by_name.insert(name, BoundLocal { local, ty });
        Ok(VixenTypedStmt::Let { local, value })
    }

    /// Lower a return-position expression, handling `If` and `Concat` specially.
    fn lower_return_expr(
        &self,
        expr: &Expr,
        bridge: &SingleSourceBridge,
        expr_ids: &HashMap<SourceSpan, ExprId>,
        locals_by_name: &HashMap<String, BoundLocal>,
        state: &mut LoweringState,
    ) -> Result<Vec<VixenTypedStmt>, StrictLoweringError> {
        let expr = &peel_expr_wrappers(expr.clone());
        match expr {
            Expr::If(if_expr) => {
                let condition = if_expr.condition().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("if without condition".into())
                })?;
                let then_branch = if_expr.then_branch().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("if without then".into())
                })?;
                let else_branch = if_expr.else_branch().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("if without else".into())
                })?;
                Ok(vec![VixenTypedStmt::If {
                    condition: self.lower_expr(&condition, bridge, expr_ids, locals_by_name)?,
                    then_body: self.lower_return_expr(
                        &then_branch,
                        bridge,
                        expr_ids,
                        locals_by_name,
                        state,
                    )?,
                    else_body: self.lower_return_expr(
                        &else_branch,
                        bridge,
                        expr_ids,
                        locals_by_name,
                        state,
                    )?,
                }])
            }
            Expr::InterpolatedString(interp) => {
                self.lower_concat_return(interp, bridge, expr_ids, locals_by_name, state)
            }
            _ => Ok(vec![VixenTypedStmt::Return(Some(self.lower_expr(
                expr,
                bridge,
                expr_ids,
                locals_by_name,
            )?))]),
        }
    }

    fn lower_expr(
        &self,
        expr: &Expr,
        bridge: &SingleSourceBridge,
        expr_ids: &HashMap<SourceSpan, ExprId>,
        locals_by_name: &HashMap<String, BoundLocal>,
    ) -> Result<VixenTypedExpr, StrictLoweringError> {
        let expr = peel_expr_wrappers(expr.clone());
        match expr {
            Expr::NameRef(name_ref) => {
                let name = name_ref
                    .token()
                    .map(|t| t.text().to_string())
                    .ok_or_else(|| StrictLoweringError::UnsupportedExpr("missing name".into()))?;
                let local = locals_by_name.get(&name).ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr(format!("unknown local `{name}`"))
                })?;
                Ok(VixenTypedExpr::Local(local.local))
            }
            Expr::Literal(lit) => self.lower_literal(&lit),
            Expr::Binary(binary) => {
                let lhs = binary.lhs().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("binary lhs missing".into())
                })?;
                let rhs = binary.rhs().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("binary rhs missing".into())
                })?;
                let op = lower_binary_op(&binary)?;
                Ok(VixenTypedExpr::Binary {
                    op,
                    lhs: Box::new(self.lower_expr(&lhs, bridge, expr_ids, locals_by_name)?),
                    rhs: Box::new(self.lower_expr(&rhs, bridge, expr_ids, locals_by_name)?),
                })
            }
            Expr::If(if_expr) => {
                let condition = if_expr.condition().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("if without condition".into())
                })?;
                let then_expr = if_expr.then_branch().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("if without then".into())
                })?;
                let else_expr = if_expr.else_branch().ok_or_else(|| {
                    StrictLoweringError::UnsupportedExpr("if without else".into())
                })?;
                Ok(VixenTypedExpr::If {
                    condition: Box::new(self.lower_expr(
                        &condition,
                        bridge,
                        expr_ids,
                        locals_by_name,
                    )?),
                    then_expr: Box::new(self.lower_expr(
                        &then_expr,
                        bridge,
                        expr_ids,
                        locals_by_name,
                    )?),
                    else_expr: Box::new(self.lower_expr(
                        &else_expr,
                        bridge,
                        expr_ids,
                        locals_by_name,
                    )?),
                })
            }
            Expr::Call(call) => self.lower_call(&call, bridge, expr_ids, locals_by_name),
            Expr::InterpolatedString(_) => {
                // Concat in non-return position is not supported by Kajit backend
                Err(StrictLoweringError::UnsupportedExpr(
                    "string interpolation in non-return position".into(),
                ))
            }
            other => Err(StrictLoweringError::UnsupportedExpr(format!("{other:?}"))),
        }
    }

    fn lower_literal(
        &self,
        lit: &vx_parser::LiteralExpr,
    ) -> Result<VixenTypedExpr, StrictLoweringError> {
        let token = lit
            .token()
            .ok_or_else(|| StrictLoweringError::UnsupportedExpr("missing literal token".into()))?;
        match token.kind() {
            SyntaxKind::INT => {
                let value = token.text().replace('_', "").parse::<u64>().map_err(|_| {
                    StrictLoweringError::UnsupportedExpr(format!("bad int `{}`", token.text()))
                })?;
                Ok(VixenTypedExpr::Literal(Literal::Integer(value)))
            }
            SyntaxKind::STRING => {
                let text = token.text();
                Ok(VixenTypedExpr::TypedLiteral {
                    literal: Literal::String(text[1..text.len() - 1].to_owned()),
                    ty: self.str_type(),
                })
            }
            SyntaxKind::TRUE_KW => Ok(VixenTypedExpr::Literal(Literal::Bool(true))),
            SyntaxKind::FALSE_KW => Ok(VixenTypedExpr::Literal(Literal::Bool(false))),
            _ => Err(StrictLoweringError::UnsupportedExpr(format!(
                "unsupported literal `{}`",
                token.text()
            ))),
        }
    }

    fn lower_call(
        &self,
        call: &vx_parser::CallExpr,
        bridge: &SingleSourceBridge,
        expr_ids: &HashMap<SourceSpan, ExprId>,
        locals_by_name: &HashMap<String, BoundLocal>,
    ) -> Result<VixenTypedExpr, StrictLoweringError> {
        let callee = call
            .callee()
            .ok_or_else(|| StrictLoweringError::UnsupportedExpr("call without callee".into()))?;
        let Expr::Field(field) = callee else {
            return Err(StrictLoweringError::UnsupportedExpr(format!(
                "non-method call `{}`",
                call.syntax().text()
            )));
        };
        let receiver_expr = field
            .receiver()
            .ok_or_else(|| StrictLoweringError::UnsupportedExpr("field receiver missing".into()))?;
        let method = field
            .field_name()
            .map(|t| t.text().to_string())
            .ok_or_else(|| StrictLoweringError::UnsupportedExpr("field name missing".into()))?;
        let receiver_ty = strict_ty_for_expr(&receiver_expr, bridge, expr_ids, locals_by_name)
            .ok_or_else(|| {
                StrictLoweringError::UnsupportedType(receiver_expr.syntax().text().to_string())
            })?;
        let receiver = self.lower_expr(&receiver_expr, bridge, expr_ids, locals_by_name)?;
        let args = call.positional_args();

        match (receiver_ty, method.as_str(), args.as_slice()) {
            (StrictTy::String, "len", []) => Ok(VixenTypedExpr::Field {
                base: Box::new(receiver),
                field: "len".to_owned(),
            }),
            (StrictTy::String, "slice", [range]) => {
                let (start, end) =
                    lower_slice_range(range, bridge, expr_ids, locals_by_name, self)?;
                Ok(VixenTypedExpr::Struct {
                    def: self.str_def,
                    fields: vec![
                        (
                            "ptr".to_owned(),
                            VixenTypedExpr::Binary {
                                op: BinaryOp::Add,
                                lhs: Box::new(VixenTypedExpr::Field {
                                    base: Box::new(receiver),
                                    field: "ptr".to_owned(),
                                }),
                                rhs: Box::new(start.clone()),
                            },
                        ),
                        (
                            "len".to_owned(),
                            VixenTypedExpr::Binary {
                                op: BinaryOp::Sub,
                                lhs: Box::new(end),
                                rhs: Box::new(start),
                            },
                        ),
                    ],
                })
            }
            _ => Err(StrictLoweringError::UnsupportedExpr(format!(
                "unsupported method `{method}` on {receiver_ty:?}"
            ))),
        }
    }

    fn lower_concat_return(
        &self,
        interp: &vx_parser::InterpolatedStringExpr,
        bridge: &SingleSourceBridge,
        expr_ids: &HashMap<SourceSpan, ExprId>,
        locals_by_name: &HashMap<String, BoundLocal>,
        state: &mut LoweringState,
    ) -> Result<Vec<VixenTypedStmt>, StrictLoweringError> {
        let mut parts = Vec::new();
        for part in interp.parts() {
            match part {
                vx_parser::StringPart::Text(token) => {
                    if !token.text().is_empty() {
                        parts.push(VixenTypedExpr::TypedLiteral {
                            literal: Literal::String(token.text().to_owned()),
                            ty: self.str_type(),
                        });
                    }
                }
                vx_parser::StringPart::Interpolation(interp) => {
                    let expr = interp.expr().ok_or_else(|| {
                        StrictLoweringError::UnsupportedExpr("missing interpolation expr".into())
                    })?;
                    parts.push(self.lower_expr(&expr, bridge, expr_ids, locals_by_name)?);
                }
            }
        }

        if parts.is_empty() {
            return Ok(vec![VixenTypedStmt::Return(Some(
                VixenTypedExpr::TypedLiteral {
                    literal: Literal::String(String::new()),
                    ty: self.str_type(),
                },
            ))]);
        }

        // Compute total length
        let len_exprs: Vec<_> = parts.iter().map(string_len_expr).collect();
        let total_len_local = state.new_temp(self, "__concat_total_len", StrictTy::U64);
        let buf_local = state.new_temp(self, "__concat_buf", StrictTy::U64);

        let mut stmts = vec![
            VixenTypedStmt::Let {
                local: total_len_local,
                value: sum_expr(len_exprs)?,
            },
            VixenTypedStmt::Let {
                local: buf_local,
                value: VixenTypedExpr::Call {
                    callee: VixenCallableRef::Named("runtime.alloc_transient".to_owned()),
                    args: vec![
                        VixenTypedExpr::Local(total_len_local),
                        VixenTypedExpr::Literal(Literal::Integer(1)),
                    ],
                },
            },
        ];

        let mut cursor = VixenTypedExpr::Local(buf_local);
        for (index, part) in parts.iter().enumerate() {
            let next = state.new_temp(self, &format!("__concat_cursor_{index}"), StrictTy::U64);
            stmts.push(VixenTypedStmt::Let {
                local: next,
                value: VixenTypedExpr::Call {
                    callee: VixenCallableRef::Named("runtime.memcpy".to_owned()),
                    args: vec![cursor, string_ptr_expr(part), string_len_expr(part)],
                },
            });
            cursor = VixenTypedExpr::Local(next);
        }

        stmts.push(VixenTypedStmt::Return(Some(VixenTypedExpr::Struct {
            def: self.str_def,
            fields: vec![
                ("ptr".to_owned(), VixenTypedExpr::Local(buf_local)),
                ("len".to_owned(), VixenTypedExpr::Local(total_len_local)),
            ],
        })));
        Ok(stmts)
    }
}

impl LoweringState {
    fn new_temp(&mut self, lowerer: &StrictLowerer, name: &str, ty: StrictTy) -> LocalId {
        let local = LocalId::new(self.next_local_id);
        self.next_local_id += 1;
        self.synthetic_locals.push(VixenTypedLocal {
            local,
            name: name.to_owned(),
            ty: lowerer.to_kajit_type(ty),
        });
        local
    }
}

// --- Helper functions ---

fn strict_ty_from_vx_ty(ty: &Ty) -> Option<StrictTy> {
    match ty {
        Ty::Scalar(Scalar::U64) => Some(StrictTy::U64),
        Ty::Scalar(Scalar::Bool) => Some(StrictTy::Bool),
        Ty::Scalar(Scalar::String) => Some(StrictTy::String),
        _ => None,
    }
}

fn strict_ty_from_parser_type(ty: &vx_parser::Type) -> Option<StrictTy> {
    let vx_parser::Type::Path(path) = ty else {
        return None;
    };
    if path.segments().count() != 1 {
        return None;
    }
    match path.segments().next()?.text() {
        "U64" => Some(StrictTy::U64),
        "Bool" => Some(StrictTy::Bool),
        "String" => Some(StrictTy::String),
        _ => None,
    }
}

fn strict_ty_for_expr(
    expr: &Expr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
) -> Option<StrictTy> {
    if let Expr::NameRef(name_ref) = expr {
        let name = name_ref.token()?.text().to_string();
        if let Some(binding) = locals_by_name.get(&name) {
            return Some(binding.ty);
        }
    }
    let span = SourceSpan::new(bridge.source_id, expr.syntax().text_range());
    let expr_id = expr_ids.get(&span)?;
    let ty = bridge.expr_types.get(expr_id)?;
    strict_ty_from_vx_ty(ty)
}

fn lower_binary_op(binary: &vx_parser::BinaryExpr) -> Result<BinaryOp, StrictLoweringError> {
    let token = binary
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .find(|token| {
            matches!(
                token.kind(),
                SyntaxKind::PLUS
                    | SyntaxKind::MINUS
                    | SyntaxKind::STAR
                    | SyntaxKind::EQ2
                    | SyntaxKind::BANG_EQ
            )
        })
        .ok_or_else(|| {
            StrictLoweringError::UnsupportedBinaryOp(binary.syntax().text().to_string())
        })?;
    match token.kind() {
        SyntaxKind::PLUS => Ok(BinaryOp::Add),
        SyntaxKind::MINUS => Ok(BinaryOp::Sub),
        SyntaxKind::STAR => Ok(BinaryOp::Mul),
        SyntaxKind::EQ2 => Ok(BinaryOp::Eq),
        SyntaxKind::BANG_EQ => Ok(BinaryOp::Ne),
        _ => Err(StrictLoweringError::UnsupportedBinaryOp(
            binary.syntax().text().to_string(),
        )),
    }
}

fn lower_slice_range(
    expr: &Expr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
    lowerer: &StrictLowerer,
) -> Result<(VixenTypedExpr, VixenTypedExpr), StrictLoweringError> {
    let expr = peel_expr_wrappers(expr.clone());
    let Expr::Range(range) = expr else {
        return Err(StrictLoweringError::UnsupportedRange(
            "expected range expression".into(),
        ));
    };
    if range.is_inclusive() {
        return Err(StrictLoweringError::UnsupportedRange(
            "inclusive range".into(),
        ));
    }
    let start = range
        .start()
        .ok_or_else(|| StrictLoweringError::UnsupportedRange("missing range start".into()))?;
    let end = range
        .end()
        .ok_or_else(|| StrictLoweringError::UnsupportedRange("missing range end".into()))?;
    Ok((
        lowerer.lower_expr(&start, bridge, expr_ids, locals_by_name)?,
        lowerer.lower_expr(&end, bridge, expr_ids, locals_by_name)?,
    ))
}

fn peel_expr_wrappers(expr: Expr) -> Expr {
    let mut current = expr;
    loop {
        current = match current {
            Expr::Paren(paren) => match paren.expr() {
                Some(inner) => inner,
                None => return Expr::Paren(paren),
            },
            Expr::Block(block) => {
                let stmts: Vec<_> = block.syntax().children().filter_map(Stmt::cast).collect();
                if stmts.len() != 1 {
                    return Expr::Block(block);
                }
                match &stmts[0] {
                    Stmt::Expr(expr) => expr.clone(),
                    Stmt::Return(ret) => match ret.expr() {
                        Some(inner) => inner,
                        None => return Expr::Block(block),
                    },
                    _ => return Expr::Block(block),
                }
            }
            other => return other,
        };
    }
}

fn string_len_expr(expr: &VixenTypedExpr) -> VixenTypedExpr {
    match expr {
        VixenTypedExpr::TypedLiteral {
            literal: Literal::String(s),
            ..
        } => VixenTypedExpr::Literal(Literal::Integer(s.len() as u64)),
        _ => VixenTypedExpr::Field {
            base: Box::new(expr.clone()),
            field: "len".to_owned(),
        },
    }
}

fn string_ptr_expr(expr: &VixenTypedExpr) -> VixenTypedExpr {
    match expr {
        VixenTypedExpr::TypedLiteral {
            literal: Literal::String(value),
            ..
        } => {
            let leaked: &'static [u8] = Box::leak(value.as_bytes().to_vec().into_boxed_slice());
            VixenTypedExpr::Literal(Literal::Integer(leaked.as_ptr() as u64))
        }
        _ => VixenTypedExpr::Field {
            base: Box::new(expr.clone()),
            field: "ptr".to_owned(),
        },
    }
}

fn sum_expr(mut exprs: Vec<VixenTypedExpr>) -> Result<VixenTypedExpr, StrictLoweringError> {
    if exprs.is_empty() {
        return Err(StrictLoweringError::UnsupportedExpr(
            "empty concatenation".into(),
        ));
    }
    let mut expr = exprs.remove(0);
    for rhs in exprs {
        expr = VixenTypedExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(expr),
            rhs: Box::new(rhs),
        };
    }
    Ok(expr)
}

fn expr_ids_by_span(bridge: &SingleSourceBridge) -> HashMap<SourceSpan, ExprId> {
    bridge
        .exprs
        .iter()
        .map(|expr| (expr.span, expr.id))
        .collect()
}

fn find_fn_decl(body_id: BodyId, bridge: &SingleSourceBridge) -> Option<FnDecl> {
    let body = bridge.bodies.iter().find(|b| b.id == body_id)?;
    let body_span = body.span;
    for (_source_id, file) in &bridge.source_files {
        for item in file.items() {
            let vx_parser::Item::FnDecl(fn_decl) = item else {
                continue;
            };
            let span = SourceSpan::new(
                vx_resolve::SourceId(*_source_id),
                fn_decl.syntax().text_range(),
            );
            if span == body_span {
                return Some(fn_decl);
            }
        }
    }
    None
}
