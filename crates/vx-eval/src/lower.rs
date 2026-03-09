//! Lowering pass: CST (rowan) → instruction tree (Send-safe).
//!
//! Walks the rowan AST once and produces owned `Expr`/`Stmt`/`FnDef`
//! structures that the evaluator can work with across threads.

use std::sync::Arc;

use vx_parser::{
    ArrayExpr, AstNode, BinaryExpr, BlockExpr, CallExpr, ClosureExpr, Expr as AstExpr, FailExpr,
    FieldExpr, FnDecl, IfExpr, Item, LiteralExpr, MapEntry, MapExpr, Parameter, PrefixExpr,
    SourceFile, Stmt as AstStmt, StructLiteralExpr, StructLiteralField, SyntaxKind,
};

use crate::instr;

#[derive(Default)]
struct LowerCtx {
    next_closure_id: usize,
}

impl LowerCtx {
    fn alloc_closure_id(&mut self) -> usize {
        let closure_id = self.next_closure_id;
        self.next_closure_id += 1;
        closure_id
    }
}

/// Lower a parsed source file into a module.
pub fn lower_source_file(source: &SourceFile) -> instr::Module {
    let mut functions = Vec::new();

    for item in source.items() {
        match item {
            Item::FnDecl(f) => {
                if let Some(def) = lower_fn_decl(&f) {
                    functions.push(Arc::new(def));
                }
            }
            Item::StructDecl(_)
            | Item::EnumDecl(_)
            | Item::NewtypeDecl(_)
            | Item::TypeAliasDecl(_) => {}
        }
    }

    instr::Module { functions }
}

fn lower_fn_decl(f: &FnDecl) -> Option<instr::FnDef> {
    let mut ctx = LowerCtx::default();
    lower_fn_decl_with_ctx(f, &mut ctx)
}

fn lower_fn_decl_with_ctx(f: &FnDecl, ctx: &mut LowerCtx) -> Option<instr::FnDef> {
    let name = f.name()?.text().to_string();
    let params: Vec<instr::Param> = f
        .parameters()
        .filter_map(|p: Parameter| {
            Some(instr::Param {
                name: p.name()?.text().to_string(),
            })
        })
        .collect();
    let body = f
        .body()
        .map(|b| lower_block_with_ctx(&b, ctx))
        .unwrap_or(instr::Block { stmts: vec![] });

    Some(instr::FnDef { name, params, body })
}

fn lower_block_with_ctx(block: &BlockExpr, ctx: &mut LowerCtx) -> instr::Block {
    let stmts = block
        .statements()
        .filter_map(|stmt| lower_stmt_with_ctx(&stmt, ctx))
        .collect();
    instr::Block { stmts }
}

fn lower_stmt_with_ctx(stmt: &AstStmt, ctx: &mut LowerCtx) -> Option<instr::Stmt> {
    match stmt {
        AstStmt::Let(l) => {
            let name = l.name()?.text().to_string();
            let value = lower_expr_with_ctx(&l.expr()?, ctx)?;
            Some(instr::Stmt::Let { name, value })
        }
        AstStmt::Assign(a) => {
            let name = a.name()?.text().to_string();
            let value = lower_expr_with_ctx(&a.expr()?, ctx)?;
            Some(instr::Stmt::Assign { name, value })
        }
        AstStmt::Return(r) => {
            let expr = r
                .expr()
                .and_then(|expr| lower_expr_with_ctx(&expr, ctx))
                .unwrap_or(instr::Expr::Unit);
            Some(instr::Stmt::Return(expr))
        }
        AstStmt::FnDecl(f) => {
            let mut fn_ctx = LowerCtx::default();
            let def = lower_fn_decl_with_ctx(f, &mut fn_ctx)?;
            Some(instr::Stmt::FnDecl(Arc::new(def)))
        }
        AstStmt::Expr(e) => {
            let expr = lower_expr_with_ctx(e, ctx)?;
            Some(instr::Stmt::Expr(expr))
        }
        AstStmt::CompoundUpdate(_) | AstStmt::FieldAssign(_) => None,
    }
}

fn lower_expr_with_ctx(expr: &AstExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    match expr {
        AstExpr::Literal(lit) => lower_literal(lit),
        AstExpr::NameRef(name_ref) => {
            let name = name_ref.token()?.text().to_string();
            Some(instr::Expr::Var(name))
        }
        AstExpr::Binary(bin) => lower_binary(bin, ctx),
        AstExpr::Prefix(pre) => lower_prefix(pre, ctx),
        AstExpr::Block(block) => Some(instr::Expr::Block(lower_block_with_ctx(block, ctx))),
        AstExpr::If(if_expr) => lower_if(if_expr, ctx),
        AstExpr::Call(call) => lower_call(call, ctx),
        AstExpr::Field(field) => lower_field(field, ctx),
        AstExpr::Fail(fail) => lower_fail(fail, ctx),
        AstExpr::StructLiteral(lit) => lower_struct_literal(lit, ctx),
        AstExpr::Array(array) => lower_array(array, ctx),
        AstExpr::Map(map) => lower_map(map, ctx),
        AstExpr::Closure(closure) => lower_closure(closure, ctx),
        AstExpr::PartialApp(_)
        | AstExpr::Index(_)
        | AstExpr::Match(_)
        | AstExpr::Range(_)
        | AstExpr::TypeOp(_)
        | AstExpr::With(_)
        | AstExpr::Fallible(_)
        | AstExpr::InterpolatedString(_) => None,
    }
}

fn lower_literal(lit: &LiteralExpr) -> Option<instr::Expr> {
    let token = lit.token()?;
    match token.kind() {
        SyntaxKind::INT => {
            let text = token.text().replace('_', "");
            let n: u64 = text.parse().ok()?;
            Some(instr::Expr::LitInt(n))
        }
        SyntaxKind::FLOAT => {
            let text = token.text().replace('_', "");
            let n: f64 = text.parse().ok()?;
            Some(instr::Expr::LitFloat(n))
        }
        SyntaxKind::TRUE_KW => Some(instr::Expr::LitBool(true)),
        SyntaxKind::FALSE_KW => Some(instr::Expr::LitBool(false)),
        SyntaxKind::STRING => {
            let text = token.text();
            let inner = &text[1..text.len() - 1];
            Some(instr::Expr::LitString(unescape_string(inner)))
        }
        _ => None,
    }
}

fn unescape_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some('0') => result.push('\0'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn lower_binary(bin: &BinaryExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let lhs = lower_expr_with_ctx(&bin.lhs()?, ctx)?;
    let rhs = lower_expr_with_ctx(&bin.rhs()?, ctx)?;

    let op = bin
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .find_map(|token| match token.kind() {
            SyntaxKind::PLUS => Some(instr::BinOp::Add),
            SyntaxKind::MINUS => Some(instr::BinOp::Sub),
            SyntaxKind::STAR => Some(instr::BinOp::Mul),
            SyntaxKind::SLASH => Some(instr::BinOp::Div),
            SyntaxKind::PERCENT => Some(instr::BinOp::Mod),
            SyntaxKind::EQ2 => Some(instr::BinOp::Eq),
            SyntaxKind::BANG_EQ => Some(instr::BinOp::Ne),
            SyntaxKind::LT => Some(instr::BinOp::Lt),
            SyntaxKind::LT_EQ => Some(instr::BinOp::Le),
            SyntaxKind::GT => Some(instr::BinOp::Gt),
            SyntaxKind::GT_EQ => Some(instr::BinOp::Ge),
            SyntaxKind::AMP2 => Some(instr::BinOp::And),
            SyntaxKind::PIPE2 => Some(instr::BinOp::Or),
            _ => None,
        })?;

    Some(instr::Expr::Binary {
        op,
        left: Box::new(lhs),
        right: Box::new(rhs),
    })
}

fn lower_prefix(pre: &PrefixExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let operand = lower_expr_with_ctx(&pre.expr()?, ctx)?;
    let op_token = pre.operator_token()?;
    let op = match op_token.kind() {
        SyntaxKind::MINUS => instr::UnaryOp::Neg,
        SyntaxKind::BANG => instr::UnaryOp::Not,
        _ => return None,
    };
    Some(instr::Expr::Unary {
        op,
        operand: Box::new(operand),
    })
}

fn lower_if(if_expr: &IfExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let cond = lower_expr_with_ctx(&if_expr.condition()?, ctx)?;
    let then_expr = if_expr.then_branch()?;
    let then_branch = match then_expr {
        AstExpr::Block(b) => lower_block_with_ctx(&b, ctx),
        other => instr::Block {
            stmts: vec![instr::Stmt::Expr(lower_expr_with_ctx(&other, ctx)?)],
        },
    };

    let else_branch = match if_expr.else_branch() {
        Some(AstExpr::Block(b)) => lower_block_with_ctx(&b, ctx),
        Some(other) => instr::Block {
            stmts: vec![instr::Stmt::Expr(lower_expr_with_ctx(&other, ctx)?)],
        },
        None => instr::Block {
            stmts: vec![instr::Stmt::Expr(instr::Expr::Unit)],
        },
    };

    Some(instr::Expr::If {
        cond: Box::new(cond),
        then_branch,
        else_branch,
    })
}

fn lower_call(call: &CallExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let callee = call.callee()?;
    let func = lower_expr_with_ctx(&callee, ctx)?;

    let args: Vec<instr::Expr> = call
        .args()
        .filter_map(|arg| arg.expr())
        .filter_map(|expr| lower_expr_with_ctx(&expr, ctx))
        .collect();

    let kwargs: Vec<(String, instr::Expr)> = call
        .keyword_args()
        .filter_map(|arg| {
            Some((
                arg.name()?.text().to_string(),
                lower_expr_with_ctx(&arg.expr()?, ctx)?,
            ))
        })
        .collect();

    Some(instr::Expr::Call {
        func: Box::new(func),
        args,
        kwargs,
    })
}

fn lower_field(field: &FieldExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let receiver = lower_expr_with_ctx(&field.receiver()?, ctx)?;
    let field_name = field.field_name()?.text().to_string();
    Some(instr::Expr::Field {
        receiver: Box::new(receiver),
        field: field_name,
    })
}

fn lower_fail(fail: &FailExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let message = lower_expr_with_ctx(&fail.message()?, ctx)?;
    Some(instr::Expr::Fail(Box::new(message)))
}

fn lower_struct_literal(lit: &StructLiteralExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let name = lit.name()?.text().to_string();
    let fields: Vec<(String, instr::Expr)> = lit
        .fields()
        .filter_map(|f: StructLiteralField| {
            let field_name = f.name()?.text().to_string();
            let value = lower_expr_with_ctx(&f.expr()?, ctx)?;
            Some((field_name, value))
        })
        .collect();

    Some(instr::Expr::StructLit { name, fields })
}

fn lower_array(array: &ArrayExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let items = array
        .items()
        .filter_map(|expr| lower_expr_with_ctx(&expr, ctx))
        .collect();
    Some(instr::Expr::Array(items))
}

fn lower_map(map: &MapExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let entries = map
        .entries()
        .filter_map(|entry: MapEntry| {
            let key = lower_expr_with_ctx(&entry.key()?, ctx)?;
            let value = lower_expr_with_ctx(&entry.value()?, ctx)?;
            Some((key, value))
        })
        .collect();
    Some(instr::Expr::Map(entries))
}

fn lower_closure(closure: &ClosureExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let closure_id = ctx.alloc_closure_id();
    let params = closure
        .parameters()
        .filter_map(|param| {
            Some(instr::Param {
                name: param.name()?.text().to_string(),
            })
        })
        .collect::<Vec<_>>();

    let mut nested_ctx = LowerCtx::default();
    let body = match closure.body()? {
        AstExpr::Block(block) => lower_block_with_ctx(&block, &mut nested_ctx),
        expr => instr::Block {
            stmts: vec![instr::Stmt::Expr(lower_expr_with_ctx(
                &expr,
                &mut nested_ctx,
            )?)],
        },
    };

    Some(instr::Expr::Closure {
        closure_id,
        def: Arc::new(instr::FnDef {
            name: format!("<closure#{closure_id}>"),
            params,
            body,
        }),
    })
}
