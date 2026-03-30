use std::collections::HashMap;

use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::{Module, VixenLoweringError};
use thiserror::Error;
use vx_analysis::{BodyId, ExprId, SingleSourceBridge, build_minimal_analysis};
use vx_parser::{AstNode, BinaryExpr, BlockExpr, Expr, FnDecl, Stmt, SyntaxKind};
use vx_resolve::{SourceId, SourceSpan};
use vx_types::{Scalar, Ty};

use crate::interpreter;
use crate::kajit_backend;
use crate::kernel::{
    KernelExpr, KernelFunction, KernelInterpretError, KernelLocalId, KernelParam, KernelType,
};

#[derive(Debug, Error)]
pub enum SourceLoweringError {
    #[error("source has parse errors: {0:?}")]
    Parse(Vec<vx_parser::SyntaxError>),
    #[error("source has resolve errors: {0:?}")]
    Resolve(Vec<vx_resolve::ResolveError>),
    #[error("source has type errors: {0:?}")]
    Type(Vec<vx_resolve::TypeError>),
    #[error("source has capability errors: {0:?}")]
    Capability(Vec<vx_resolve::CapError>),
    #[error("analysis validation failed: {0:?}")]
    Analysis(vx_analysis::AnalysisError),
    #[error("missing function `{0}`")]
    MissingFunction(String),
    #[error("missing AST body for function `{0}`")]
    MissingFunctionAst(String),
    #[error("unsupported parameter list in `{0}`")]
    UnsupportedParameters(String),
    #[error("unsupported return type in `{0}`")]
    UnsupportedReturnType(String),
    #[error("unsupported body shape in `{0}`: {1}")]
    UnsupportedBody(String, String),
    #[error("unsupported expression: {0}")]
    UnsupportedExpr(String),
    #[error("unsupported binary operator in `{0}`")]
    UnsupportedBinaryOperator(String),
    #[error("missing expression id for `{0}`")]
    MissingExprId(String),
    #[error("missing expression type for `{0}`")]
    MissingExprType(String),
    #[error("expression `{0}` is not U64")]
    ExprNotU64(String),
    #[error("unknown local `{0}`")]
    UnknownLocal(String),
    #[error("interpreter failed: {0}")]
    Interpret(KernelInterpretError),
    #[error("kajit lowering failed: {0:?}")]
    KajitLowering(VixenLoweringError),
}

pub fn lower_source_text_u64_add_kernel(
    source: &str,
    function_name: &str,
) -> Result<KernelFunction, SourceLoweringError> {
    let bridge = checked_bridge(source)?;
    lower_bridge_function_u64_add(&bridge, function_name)
}

pub fn lower_source_text_u64_add_hir(
    source: &str,
    function_name: &str,
) -> Result<Module, SourceLoweringError> {
    lower_source_text_u64_add_kernel(source, function_name).and_then(|function| {
        kajit_backend::lower_hir(&function).map_err(SourceLoweringError::KajitLowering)
    })
}

pub fn lower_source_text_u64_add_ir(
    source: &str,
    function_name: &str,
) -> Result<IrFunc, SourceLoweringError> {
    lower_source_text_u64_add_kernel(source, function_name).and_then(|function| {
        kajit_backend::lower_ir(&function).map_err(SourceLoweringError::KajitLowering)
    })
}

pub fn compile_source_text_u64_add_function(
    source: &str,
    function_name: &str,
) -> Result<CompiledFunction, SourceLoweringError> {
    lower_source_text_u64_add_kernel(source, function_name).and_then(|function| {
        kajit_backend::compile(&function).map_err(SourceLoweringError::KajitLowering)
    })
}

pub fn interpret_source_text_u64_add_function(
    source: &str,
    function_name: &str,
    lhs: u64,
    rhs: u64,
) -> Result<u64, SourceLoweringError> {
    let function = lower_source_text_u64_add_kernel(source, function_name)?;
    interpreter::interpret_u64(&function, &[lhs, rhs]).map_err(SourceLoweringError::Interpret)
}

pub unsafe fn run_source_text_u64_add_function(
    source: &str,
    function_name: &str,
    lhs: u64,
    rhs: u64,
) -> Result<u64, SourceLoweringError> {
    let compiled = compile_source_text_u64_add_function(source, function_name)?;
    let add: unsafe extern "C" fn(u64, u64) -> u64 =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    Ok(unsafe { add(lhs, rhs) })
}

fn checked_bridge(source: &str) -> Result<SingleSourceBridge, SourceLoweringError> {
    let bridge = SingleSourceBridge::from_source_text(source);

    if !bridge.parse_errors.is_empty() {
        return Err(SourceLoweringError::Parse(bridge.parse_errors.clone()));
    }
    if !bridge.resolve_errors.is_empty() {
        return Err(SourceLoweringError::Resolve(bridge.resolve_errors.clone()));
    }
    if !bridge.type_errors.is_empty() {
        return Err(SourceLoweringError::Type(bridge.type_errors.clone()));
    }
    if !bridge.cap_errors.is_empty() {
        return Err(SourceLoweringError::Capability(bridge.cap_errors.clone()));
    }

    build_minimal_analysis(&bridge)
        .validate()
        .map_err(SourceLoweringError::Analysis)?;

    Ok(bridge)
}

fn lower_bridge_function_u64_add(
    bridge: &SingleSourceBridge,
    function_name: &str,
) -> Result<KernelFunction, SourceLoweringError> {
    let body = bridge
        .bodies
        .iter()
        .find(|body| body.name == function_name)
        .ok_or_else(|| SourceLoweringError::MissingFunction(function_name.to_owned()))?;
    let fn_decl = body_nodes_by_id(bridge)
        .get(&body.id)
        .cloned()
        .ok_or_else(|| SourceLoweringError::MissingFunctionAst(function_name.to_owned()))?;
    let expr_ids = expr_ids_by_span(bridge);

    let mut locals_by_name = HashMap::new();
    let mut params = Vec::new();
    for (index, param) in fn_decl.parameters().enumerate() {
        let name = param
            .name()
            .map(|token| token.text().to_string())
            .ok_or_else(|| SourceLoweringError::UnsupportedParameters(function_name.to_owned()))?;
        let ty = param
            .slot_type()
            .and_then(|slot| slot.ty())
            .ok_or_else(|| SourceLoweringError::UnsupportedParameters(function_name.to_owned()))?;
        if !is_u64_type(&ty) {
            return Err(SourceLoweringError::UnsupportedParameters(
                function_name.to_owned(),
            ));
        }

        let local = KernelLocalId(index as u32);
        params.push(KernelParam {
            local,
            name: name.clone(),
            ty: KernelType::U64,
        });
        locals_by_name.insert(name, local);
    }

    let return_ty = fn_decl
        .return_type()
        .and_then(|ret| ret.ty())
        .ok_or_else(|| SourceLoweringError::UnsupportedReturnType(function_name.to_owned()))?;
    if !is_u64_type(&return_ty) {
        return Err(SourceLoweringError::UnsupportedReturnType(
            function_name.to_owned(),
        ));
    }

    let body_expr = body_result_expr(&fn_decl, function_name)?;
    let lowered_expr = lower_expr_u64(&body_expr, bridge, &expr_ids, &locals_by_name)?;

    Ok(KernelFunction {
        name: function_name.to_owned(),
        params,
        return_type: KernelType::U64,
        body: lowered_expr,
    })
}

fn body_result_expr(fn_decl: &FnDecl, function_name: &str) -> Result<Expr, SourceLoweringError> {
    if let Some(expr) = fn_decl.body_expr() {
        return match expr {
            Expr::Block(block) => body_result_expr_from_block(&block, function_name),
            Expr::Paren(paren) => paren.expr().ok_or_else(|| {
                SourceLoweringError::UnsupportedBody(
                    function_name.to_owned(),
                    "parenthesized body missing inner expression".to_owned(),
                )
            }),
            other => Ok(other),
        };
    }

    let block = fn_decl.body().ok_or_else(|| {
        SourceLoweringError::UnsupportedBody(
            function_name.to_owned(),
            "missing block body".to_owned(),
        )
    })?;
    body_result_expr_from_block(&block, function_name)
}

fn body_result_expr_from_block(
    block: &BlockExpr,
    function_name: &str,
) -> Result<Expr, SourceLoweringError> {
    let statements = block
        .syntax()
        .children()
        .filter_map(Stmt::cast)
        .collect::<Vec<_>>();
    let exprs = block
        .syntax()
        .children()
        .filter_map(Expr::cast)
        .collect::<Vec<_>>();

    if exprs.len() == 1 {
        return Ok(exprs[0].clone());
    }

    if exprs.is_empty() && statements.len() == 1 {
        if let Stmt::Return(ret) = &statements[0] {
            return ret.expr().ok_or_else(|| {
                SourceLoweringError::UnsupportedBody(
                    function_name.to_owned(),
                    "return without expression".to_owned(),
                )
            });
        }
    }

    Err(SourceLoweringError::UnsupportedBody(
        function_name.to_owned(),
        "expected a single tail expression or a single return statement".to_owned(),
    ))
}

fn lower_expr_u64(
    expr: &Expr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, KernelLocalId>,
) -> Result<KernelExpr, SourceLoweringError> {
    if let Expr::Paren(paren) = expr {
        let inner = paren.expr().ok_or_else(|| {
            SourceLoweringError::UnsupportedExpr(
                "parenthesized expression missing inner expr".to_owned(),
            )
        })?;
        return lower_expr_u64(&inner, bridge, expr_ids, locals_by_name);
    }

    ensure_expr_is_u64(expr, bridge, expr_ids)?;

    match expr {
        Expr::NameRef(name_ref) => {
            let name = name_ref
                .token()
                .map(|token| token.text().to_string())
                .ok_or_else(|| {
                    SourceLoweringError::UnsupportedExpr("missing name ref token".to_owned())
                })?;
            let local = locals_by_name
                .get(&name)
                .copied()
                .ok_or_else(|| SourceLoweringError::UnknownLocal(name.clone()))?;
            Ok(KernelExpr::Local(local))
        }
        Expr::Literal(literal) => {
            let token = literal.token().ok_or_else(|| {
                SourceLoweringError::UnsupportedExpr("missing literal token".to_owned())
            })?;
            if token.kind() != SyntaxKind::INT {
                return Err(SourceLoweringError::UnsupportedExpr(format!(
                    "only INT literals are supported, got `{}`",
                    token.text()
                )));
            }
            let value = token.text().replace('_', "").parse::<u64>().map_err(|_| {
                SourceLoweringError::UnsupportedExpr(format!(
                    "invalid integer literal `{}`",
                    token.text()
                ))
            })?;
            Ok(KernelExpr::U64(value))
        }
        Expr::Binary(binary) => lower_binary_u64(binary, bridge, expr_ids, locals_by_name),
        other => Err(SourceLoweringError::UnsupportedExpr(format!(
            "unsupported expression variant `{other:?}`"
        ))),
    }
}

fn lower_binary_u64(
    binary: &BinaryExpr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, KernelLocalId>,
) -> Result<KernelExpr, SourceLoweringError> {
    let has_plus = binary
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .any(|token| token.kind() == SyntaxKind::PLUS);
    if !has_plus {
        return Err(SourceLoweringError::UnsupportedBinaryOperator(
            binary.syntax().text().to_string(),
        ));
    }

    let lhs = binary
        .lhs()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("binary lhs missing".to_owned()))?;
    let rhs = binary
        .rhs()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("binary rhs missing".to_owned()))?;

    Ok(KernelExpr::Add(
        Box::new(lower_expr_u64(&lhs, bridge, expr_ids, locals_by_name)?),
        Box::new(lower_expr_u64(&rhs, bridge, expr_ids, locals_by_name)?),
    ))
}

fn ensure_expr_is_u64(
    expr: &Expr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
) -> Result<(), SourceLoweringError> {
    let expr_id = expr_id_for_expr(bridge.source_id, expr, expr_ids)?;
    let ty = bridge
        .expr_types
        .get(&expr_id)
        .ok_or_else(|| SourceLoweringError::MissingExprType(expr.syntax().text().to_string()))?;
    if *ty != Ty::Scalar(Scalar::U64) {
        return Err(SourceLoweringError::ExprNotU64(
            expr.syntax().text().to_string(),
        ));
    }
    Ok(())
}

fn expr_id_for_expr(
    source_id: SourceId,
    expr: &Expr,
    expr_ids: &HashMap<SourceSpan, ExprId>,
) -> Result<ExprId, SourceLoweringError> {
    let span = SourceSpan::new(source_id, expr.syntax().text_range());
    expr_ids
        .get(&span)
        .copied()
        .ok_or_else(|| SourceLoweringError::MissingExprId(expr.syntax().text().to_string()))
}

fn expr_ids_by_span(bridge: &SingleSourceBridge) -> HashMap<SourceSpan, ExprId> {
    bridge
        .exprs
        .iter()
        .map(|expr| (expr.span, expr.id))
        .collect()
}

fn body_nodes_by_id(bridge: &SingleSourceBridge) -> HashMap<BodyId, FnDecl> {
    let body_by_span = bridge
        .bodies
        .iter()
        .map(|body| (body.span, body.id))
        .collect::<HashMap<_, _>>();

    let mut fn_by_body = HashMap::new();
    for (source_id, file) in &bridge.source_files {
        for item in file.items() {
            let vx_parser::Item::FnDecl(fn_decl) = item else {
                continue;
            };
            let span = SourceSpan::new(SourceId(*source_id), fn_decl.syntax().text_range());
            if let Some(body_id) = body_by_span.get(&span).copied() {
                fn_by_body.insert(body_id, fn_decl);
            }
        }
    }
    fn_by_body
}

fn is_u64_type(ty: &vx_parser::Type) -> bool {
    let vx_parser::Type::Path(path) = ty else {
        return false;
    };
    path.segments().count() == 1
        && path
            .segments()
            .next()
            .is_some_and(|segment| segment.text() == "U64")
}
