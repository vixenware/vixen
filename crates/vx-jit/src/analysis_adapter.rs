use std::collections::HashMap;

use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::Module;
use thiserror::Error;
use vx_analysis::{BodyId, ExprId, SingleSourceBridge, build_minimal_analysis};
use vx_parser::{
    AstNode, BinaryExpr, BlockExpr, CallExpr, Expr, FnDecl, IfExpr, InterpolatedStringExpr,
    LetStmt, LiteralExpr, Stmt, SyntaxKind,
};
use vx_resolve::{SourceId, SourceSpan};
use vx_types::{Scalar, Ty};

use crate::interpreter;
use crate::kajit_backend::{self, KajitBackendError};
use crate::kernel::{
    KernelBinaryOp, KernelExpr, KernelFunction, KernelInterpretError, KernelIntrinsic, KernelLocal,
    KernelLocalId, KernelParam, KernelStmt, KernelType, KernelValue,
};

#[derive(Debug, Clone, Copy)]
struct BoundLocal {
    local: KernelLocalId,
    ty: KernelType,
}

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
    #[error("unsupported statement in `{0}`: {1}")]
    UnsupportedStatement(String, String),
    #[error("unsupported pattern in `{0}`")]
    UnsupportedPattern(String),
    #[error("unsupported expression: {0}")]
    UnsupportedExpr(String),
    #[error("unsupported binary operator in `{0}`")]
    UnsupportedBinaryOperator(String),
    #[error("unsupported method `{method}` on {receiver:?}")]
    UnsupportedMethod {
        method: String,
        receiver: KernelType,
    },
    #[error("unsupported range expression `{0}`")]
    UnsupportedRange(String),
    #[error("missing expression id for `{0}`")]
    MissingExprId(String),
    #[error("missing expression type for `{0}`")]
    MissingExprType(String),
    #[error("unknown local `{0}`")]
    UnknownLocal(String),
    #[error("unsupported type in `{0}`")]
    UnsupportedType(String),
    #[error("interpreter failed: {0}")]
    Interpret(KernelInterpretError),
    #[error("Kajit backend failed: {0}")]
    Kajit(#[from] KajitBackendError),
}

pub fn lower_source_text_kernel(
    source: &str,
    function_name: &str,
) -> Result<KernelFunction, SourceLoweringError> {
    let bridge = checked_bridge(source)?;
    lower_bridge_function(&bridge, function_name)
}

pub fn lower_source_text_u64_add_kernel(
    source: &str,
    function_name: &str,
) -> Result<KernelFunction, SourceLoweringError> {
    lower_source_text_kernel(source, function_name)
}

pub fn lower_source_text_u64_add_hir(
    source: &str,
    function_name: &str,
) -> Result<Module, SourceLoweringError> {
    lower_source_text_hir(source, function_name)
}

pub fn lower_source_text_hir(
    source: &str,
    function_name: &str,
) -> Result<Module, SourceLoweringError> {
    let function = lower_source_text_kernel(source, function_name)?;
    kajit_backend::lower_hir(&function).map_err(SourceLoweringError::Kajit)
}

pub fn lower_source_text_u64_add_ir(
    source: &str,
    function_name: &str,
) -> Result<IrFunc, SourceLoweringError> {
    lower_source_text_ir(source, function_name)
}

pub fn lower_source_text_ir(
    source: &str,
    function_name: &str,
) -> Result<IrFunc, SourceLoweringError> {
    let function = lower_source_text_kernel(source, function_name)?;
    kajit_backend::lower_ir(&function).map_err(SourceLoweringError::Kajit)
}

pub fn compile_source_text_u64_add_function(
    source: &str,
    function_name: &str,
) -> Result<CompiledFunction, SourceLoweringError> {
    compile_source_text_function(source, function_name)
}

pub fn compile_source_text_function(
    source: &str,
    function_name: &str,
) -> Result<CompiledFunction, SourceLoweringError> {
    let function = lower_source_text_kernel(source, function_name)?;
    kajit_backend::compile(&function).map_err(SourceLoweringError::Kajit)
}

pub fn interpret_source_text_function(
    source: &str,
    function_name: &str,
    args: &[KernelValue],
) -> Result<KernelValue, SourceLoweringError> {
    let function = lower_source_text_kernel(source, function_name)?;
    interpreter::interpret(&function, args).map_err(SourceLoweringError::Interpret)
}

pub fn interpret_source_text_string_function(
    source: &str,
    function_name: &str,
    args: &[KernelValue],
) -> Result<String, SourceLoweringError> {
    let function = lower_source_text_kernel(source, function_name)?;
    interpreter::interpret_string(&function, args).map_err(SourceLoweringError::Interpret)
}

pub fn interpret_source_text_u64_add_function(
    source: &str,
    function_name: &str,
    lhs: u64,
    rhs: u64,
) -> Result<u64, SourceLoweringError> {
    let function = lower_source_text_kernel(source, function_name)?;
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

fn lower_bridge_function(
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
    let mut next_local = 0u32;
    let params = lower_params(
        &fn_decl,
        function_name,
        &mut locals_by_name,
        &mut next_local,
    )?;
    let return_type = lower_return_type(&fn_decl, function_name)?;
    let body = lower_function_body(
        &fn_decl,
        bridge,
        &expr_ids,
        function_name,
        &mut locals_by_name,
        &mut next_local,
    )?;

    Ok(KernelFunction {
        name: function_name.to_owned(),
        params,
        return_type,
        body,
    })
}

fn lower_params(
    fn_decl: &FnDecl,
    function_name: &str,
    locals_by_name: &mut HashMap<String, BoundLocal>,
    next_local: &mut u32,
) -> Result<Vec<KernelParam>, SourceLoweringError> {
    let mut params = Vec::new();
    for param in fn_decl.parameters() {
        let name = param
            .name()
            .map(|token| token.text().to_string())
            .ok_or_else(|| SourceLoweringError::UnsupportedParameters(function_name.to_owned()))?;
        let ty = param
            .slot_type()
            .and_then(|slot| slot.ty())
            .ok_or_else(|| SourceLoweringError::UnsupportedParameters(function_name.to_owned()))?;
        let ty = kernel_type_from_parser_type(&ty)
            .ok_or_else(|| SourceLoweringError::UnsupportedParameters(function_name.to_owned()))?;
        let local = KernelLocalId(*next_local);
        *next_local += 1;
        params.push(KernelParam {
            local,
            name: name.clone(),
            ty,
        });
        locals_by_name.insert(name, BoundLocal { local, ty });
    }
    Ok(params)
}

fn lower_return_type(
    fn_decl: &FnDecl,
    function_name: &str,
) -> Result<KernelType, SourceLoweringError> {
    let return_ty = fn_decl
        .return_type()
        .and_then(|ret| ret.ty())
        .ok_or_else(|| SourceLoweringError::UnsupportedReturnType(function_name.to_owned()))?;
    kernel_type_from_parser_type(&return_ty)
        .ok_or_else(|| SourceLoweringError::UnsupportedReturnType(function_name.to_owned()))
}

fn lower_function_body(
    fn_decl: &FnDecl,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    function_name: &str,
    locals_by_name: &mut HashMap<String, BoundLocal>,
    next_local: &mut u32,
) -> Result<Vec<KernelStmt>, SourceLoweringError> {
    if let Some(block) = fn_decl.body() {
        return lower_block_body(
            &block,
            bridge,
            expr_ids,
            function_name,
            locals_by_name,
            next_local,
        );
    }

    let body_expr = fn_decl.body_expr().ok_or_else(|| {
        SourceLoweringError::UnsupportedBody(function_name.to_owned(), "missing body".to_owned())
    })?;
    let body_expr = peel_expr_wrappers(body_expr, function_name)?;
    Ok(vec![KernelStmt::Return(lower_expr(
        &body_expr,
        bridge,
        expr_ids,
        locals_by_name,
        function_name,
    )?)])
}

fn lower_block_body(
    block: &BlockExpr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    function_name: &str,
    locals_by_name: &mut HashMap<String, BoundLocal>,
    next_local: &mut u32,
) -> Result<Vec<KernelStmt>, SourceLoweringError> {
    let statements = block
        .syntax()
        .children()
        .filter_map(Stmt::cast)
        .collect::<Vec<_>>();

    let mut out = Vec::new();
    for (index, stmt) in statements.iter().enumerate() {
        let is_last = index + 1 == statements.len();
        lower_stmt(
            stmt,
            is_last,
            bridge,
            expr_ids,
            function_name,
            locals_by_name,
            next_local,
            &mut out,
        )?;
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn lower_stmt(
    stmt: &Stmt,
    is_last: bool,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    function_name: &str,
    locals_by_name: &mut HashMap<String, BoundLocal>,
    next_local: &mut u32,
    out: &mut Vec<KernelStmt>,
) -> Result<(), SourceLoweringError> {
    match stmt {
        Stmt::Let(let_stmt) => {
            out.push(lower_let_stmt(
                let_stmt,
                bridge,
                expr_ids,
                function_name,
                locals_by_name,
                next_local,
            )?);
            Ok(())
        }
        Stmt::Return(ret) => {
            let expr = ret.expr().ok_or_else(|| {
                SourceLoweringError::UnsupportedStatement(
                    function_name.to_owned(),
                    "return without expression".to_owned(),
                )
            })?;
            let expr = peel_expr_wrappers(expr, function_name)?;
            out.push(KernelStmt::Return(lower_expr(
                &expr,
                bridge,
                expr_ids,
                locals_by_name,
                function_name,
            )?));
            Ok(())
        }
        Stmt::Expr(expr) if is_last => {
            let expr = peel_expr_wrappers(expr.clone(), function_name)?;
            out.push(KernelStmt::Return(lower_expr(
                &expr,
                bridge,
                expr_ids,
                locals_by_name,
                function_name,
            )?));
            Ok(())
        }
        Stmt::Expr(expr) => Err(SourceLoweringError::UnsupportedStatement(
            function_name.to_owned(),
            format!("non-tail expression statement `{}`", expr.syntax().text()),
        )),
        other => Err(SourceLoweringError::UnsupportedStatement(
            function_name.to_owned(),
            format!("unsupported statement variant `{other:?}`"),
        )),
    }
}

fn lower_let_stmt(
    let_stmt: &LetStmt,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    function_name: &str,
    locals_by_name: &mut HashMap<String, BoundLocal>,
    next_local: &mut u32,
) -> Result<KernelStmt, SourceLoweringError> {
    let Some(vx_parser::Pattern::Binding(pattern)) = let_stmt.pattern() else {
        return Err(SourceLoweringError::UnsupportedPattern(
            function_name.to_owned(),
        ));
    };
    let name = pattern
        .name()
        .map(|token| token.text().to_string())
        .ok_or_else(|| SourceLoweringError::UnsupportedPattern(function_name.to_owned()))?;
    let expr = let_stmt.expr().ok_or_else(|| {
        SourceLoweringError::UnsupportedStatement(
            function_name.to_owned(),
            "let without expression".to_owned(),
        )
    })?;
    let expr = peel_expr_wrappers(expr, function_name)?;
    let ty = let_stmt
        .slot_type()
        .and_then(|slot| slot.ty())
        .and_then(|ty| kernel_type_from_parser_type(&ty))
        .or_else(|| kernel_type_for_expr(&expr, bridge, expr_ids, locals_by_name))
        .ok_or_else(|| SourceLoweringError::UnsupportedType(expr.syntax().text().to_string()))?;
    let local = KernelLocal {
        local: KernelLocalId(*next_local),
        name: name.clone(),
        ty,
    };
    *next_local += 1;
    let value = lower_expr(&expr, bridge, expr_ids, locals_by_name, function_name)?;
    locals_by_name.insert(
        name,
        BoundLocal {
            local: local.local,
            ty: local.ty,
        },
    );
    Ok(KernelStmt::Let { local, value })
}

fn lower_expr(
    expr: &Expr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
    function_name: &str,
) -> Result<KernelExpr, SourceLoweringError> {
    let expr = peel_expr_wrappers(expr.clone(), function_name)?;

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
                .map(|binding| binding.local)
                .ok_or_else(|| SourceLoweringError::UnknownLocal(name.clone()))?;
            Ok(KernelExpr::Local(local))
        }
        Expr::Literal(literal) => lower_literal_expr(&literal),
        Expr::Binary(binary) => {
            lower_binary_expr(&binary, bridge, expr_ids, locals_by_name, function_name)
        }
        Expr::If(if_expr) => {
            lower_if_expr(&if_expr, bridge, expr_ids, locals_by_name, function_name)
        }
        Expr::Call(call) => lower_call_expr(&call, bridge, expr_ids, locals_by_name, function_name),
        Expr::InterpolatedString(interp) => {
            lower_interpolated_string(&interp, bridge, expr_ids, locals_by_name, function_name)
        }
        other => Err(SourceLoweringError::UnsupportedExpr(format!(
            "unsupported expression variant `{other:?}`"
        ))),
    }
}

fn lower_literal_expr(literal: &LiteralExpr) -> Result<KernelExpr, SourceLoweringError> {
    let token = literal
        .token()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("missing literal token".to_owned()))?;
    match token.kind() {
        SyntaxKind::INT => {
            let value = token.text().replace('_', "").parse::<u64>().map_err(|_| {
                SourceLoweringError::UnsupportedExpr(format!(
                    "invalid integer literal `{}`",
                    token.text()
                ))
            })?;
            Ok(KernelExpr::U64(value))
        }
        SyntaxKind::STRING => {
            let text = token.text();
            Ok(KernelExpr::String(text[1..text.len() - 1].to_owned()))
        }
        SyntaxKind::TRUE_KW => Ok(KernelExpr::Bool(true)),
        SyntaxKind::FALSE_KW => Ok(KernelExpr::Bool(false)),
        _ => Err(SourceLoweringError::UnsupportedExpr(format!(
            "unsupported literal `{}`",
            token.text()
        ))),
    }
}

fn lower_binary_expr(
    binary: &BinaryExpr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
    function_name: &str,
) -> Result<KernelExpr, SourceLoweringError> {
    let lhs = binary
        .lhs()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("binary lhs missing".to_owned()))?;
    let rhs = binary
        .rhs()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("binary rhs missing".to_owned()))?;
    let op = lower_binary_op(binary)?;
    Ok(KernelExpr::Binary {
        op,
        lhs: Box::new(lower_expr(
            &lhs,
            bridge,
            expr_ids,
            locals_by_name,
            function_name,
        )?),
        rhs: Box::new(lower_expr(
            &rhs,
            bridge,
            expr_ids,
            locals_by_name,
            function_name,
        )?),
    })
}

fn lower_binary_op(binary: &BinaryExpr) -> Result<KernelBinaryOp, SourceLoweringError> {
    let token = binary
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .find(|token| matches!(token.kind(), SyntaxKind::PLUS | SyntaxKind::EQ2))
        .ok_or_else(|| {
            SourceLoweringError::UnsupportedBinaryOperator(binary.syntax().text().to_string())
        })?;

    match token.kind() {
        SyntaxKind::PLUS => Ok(KernelBinaryOp::Add),
        SyntaxKind::EQ2 => Ok(KernelBinaryOp::Eq),
        _ => Err(SourceLoweringError::UnsupportedBinaryOperator(
            binary.syntax().text().to_string(),
        )),
    }
}

fn lower_if_expr(
    if_expr: &IfExpr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
    function_name: &str,
) -> Result<KernelExpr, SourceLoweringError> {
    let condition = if_expr
        .condition()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("if condition missing".to_owned()))?;
    let then_expr = if_expr
        .then_branch()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("if then branch missing".to_owned()))?;
    let else_expr = if_expr
        .else_branch()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("if else branch missing".to_owned()))?;

    Ok(KernelExpr::If {
        condition: Box::new(lower_expr(
            &condition,
            bridge,
            expr_ids,
            locals_by_name,
            function_name,
        )?),
        then_expr: Box::new(lower_expr(
            &then_expr,
            bridge,
            expr_ids,
            locals_by_name,
            function_name,
        )?),
        else_expr: Box::new(lower_expr(
            &else_expr,
            bridge,
            expr_ids,
            locals_by_name,
            function_name,
        )?),
    })
}

fn lower_call_expr(
    call: &CallExpr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
    function_name: &str,
) -> Result<KernelExpr, SourceLoweringError> {
    let callee = call
        .callee()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("call without callee".to_owned()))?;
    let Expr::Field(field) = callee else {
        return Err(SourceLoweringError::UnsupportedExpr(format!(
            "unsupported call callee `{}`",
            call.syntax().text()
        )));
    };
    let receiver_expr = field
        .receiver()
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("field receiver missing".to_owned()))?;
    let method = field
        .field_name()
        .map(|token| token.text().to_string())
        .ok_or_else(|| SourceLoweringError::UnsupportedExpr("field name missing".to_owned()))?;
    let receiver_type = kernel_type_for_expr(&receiver_expr, bridge, expr_ids, locals_by_name)
        .ok_or_else(|| {
            SourceLoweringError::UnsupportedType(receiver_expr.syntax().text().to_string())
        })?;
    let receiver = lower_expr(
        &receiver_expr,
        bridge,
        expr_ids,
        locals_by_name,
        function_name,
    )?;
    let args = call.positional_args();

    match (receiver_type, method.as_str(), args.as_slice()) {
        (KernelType::String, "len", []) => Ok(KernelExpr::Intrinsic {
            intrinsic: KernelIntrinsic::StringLen,
            args: vec![receiver],
        }),
        (KernelType::String, "slice", [range]) => {
            let (start, end) =
                lower_slice_range(range, bridge, expr_ids, locals_by_name, function_name)?;
            Ok(KernelExpr::Intrinsic {
                intrinsic: KernelIntrinsic::StringSlice,
                args: vec![receiver, start, end],
            })
        }
        _ => Err(SourceLoweringError::UnsupportedMethod {
            method,
            receiver: receiver_type,
        }),
    }
}

fn lower_slice_range(
    expr: &Expr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
    function_name: &str,
) -> Result<(KernelExpr, KernelExpr), SourceLoweringError> {
    let Expr::Range(range) = peel_expr_wrappers(expr.clone(), function_name)? else {
        return Err(SourceLoweringError::UnsupportedRange(
            expr.syntax().text().to_string(),
        ));
    };
    if range.is_inclusive() {
        return Err(SourceLoweringError::UnsupportedRange(
            expr.syntax().text().to_string(),
        ));
    }
    let start = range
        .start()
        .ok_or_else(|| SourceLoweringError::UnsupportedRange(expr.syntax().text().to_string()))?;
    let end = range
        .end()
        .ok_or_else(|| SourceLoweringError::UnsupportedRange(expr.syntax().text().to_string()))?;
    Ok((
        lower_expr(&start, bridge, expr_ids, locals_by_name, function_name)?,
        lower_expr(&end, bridge, expr_ids, locals_by_name, function_name)?,
    ))
}

fn lower_interpolated_string(
    interp: &InterpolatedStringExpr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
    function_name: &str,
) -> Result<KernelExpr, SourceLoweringError> {
    let mut parts = Vec::new();
    for part in interp.parts() {
        match part {
            vx_parser::StringPart::Text(token) => {
                if !token.text().is_empty() {
                    parts.push(KernelExpr::String(token.text().to_owned()));
                }
            }
            vx_parser::StringPart::Interpolation(interp) => {
                let expr = interp.expr().ok_or_else(|| {
                    SourceLoweringError::UnsupportedExpr(
                        "string interpolation missing expression".to_owned(),
                    )
                })?;
                parts.push(lower_expr(
                    &expr,
                    bridge,
                    expr_ids,
                    locals_by_name,
                    function_name,
                )?);
            }
        }
    }
    Ok(KernelExpr::Concat(parts))
}

fn peel_expr_wrappers(expr: Expr, function_name: &str) -> Result<Expr, SourceLoweringError> {
    let mut current = expr;
    loop {
        current = match current {
            Expr::Paren(paren) => paren.expr().ok_or_else(|| {
                SourceLoweringError::UnsupportedBody(
                    function_name.to_owned(),
                    "parenthesized expression missing inner expression".to_owned(),
                )
            })?,
            Expr::Block(block) => {
                let stmts = block
                    .syntax()
                    .children()
                    .filter_map(Stmt::cast)
                    .collect::<Vec<_>>();
                if stmts.len() != 1 {
                    return Ok(Expr::Block(block));
                }
                match &stmts[0] {
                    Stmt::Expr(expr) => expr.clone(),
                    Stmt::Return(ret) => ret.expr().ok_or_else(|| {
                        SourceLoweringError::UnsupportedBody(
                            function_name.to_owned(),
                            "return without expression".to_owned(),
                        )
                    })?,
                    _ => return Ok(Expr::Block(block)),
                }
            }
            other => return Ok(other),
        };
    }
}

fn kernel_type_for_expr(
    expr: &Expr,
    bridge: &SingleSourceBridge,
    expr_ids: &HashMap<SourceSpan, ExprId>,
    locals_by_name: &HashMap<String, BoundLocal>,
) -> Option<KernelType> {
    if let Expr::NameRef(name_ref) = expr {
        let name = name_ref.token()?.text().to_string();
        if let Some(binding) = locals_by_name.get(&name) {
            return Some(binding.ty);
        }
    }
    let expr_id = expr_id_for_expr(bridge.source_id, expr, expr_ids).ok()?;
    let ty = bridge.expr_types.get(&expr_id)?;
    kernel_type_from_ty(ty)
}

fn kernel_type_from_parser_type(ty: &vx_parser::Type) -> Option<KernelType> {
    let vx_parser::Type::Path(path) = ty else {
        return None;
    };
    if path.segments().count() != 1 {
        return None;
    }
    match path.segments().next()?.text() {
        "U64" => Some(KernelType::U64),
        "Bool" => Some(KernelType::Bool),
        "String" => Some(KernelType::String),
        _ => None,
    }
}

fn kernel_type_from_ty(ty: &Ty) -> Option<KernelType> {
    match ty {
        Ty::Scalar(Scalar::U64) => Some(KernelType::U64),
        Ty::Scalar(Scalar::Bool) => Some(KernelType::Bool),
        Ty::Scalar(Scalar::String) => Some(KernelType::String),
        _ => None,
    }
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
