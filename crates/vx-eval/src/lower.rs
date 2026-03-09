//! Lowering pass: CST (rowan) → instruction tree (Send-safe).
//!
//! Walks the rowan AST once and produces owned `Expr`/`Stmt`/`FnDef`
//! structures that the evaluator can work with across threads.

use std::sync::Arc;

use vx_parser::{
    AstNode, BinaryExpr, BlockExpr, CallExpr, Expr as AstExpr, FieldExpr, FnDecl, IfExpr, Item,
    LiteralExpr, Parameter, PrefixExpr, SourceFile, Stmt as AstStmt, StructLiteralExpr,
    StructLiteralField, SyntaxKind,
};

use crate::instr;

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
        .map(|b| lower_block(&b))
        .unwrap_or(instr::Block { stmts: vec![] });

    Some(instr::FnDef { name, params, body })
}

fn lower_block(block: &BlockExpr) -> instr::Block {
    let stmts = block.statements().filter_map(|s| lower_stmt(&s)).collect();
    instr::Block { stmts }
}

fn lower_stmt(stmt: &AstStmt) -> Option<instr::Stmt> {
    match stmt {
        AstStmt::Let(l) => {
            let name = l.name()?.text().to_string();
            let value = lower_expr(&l.expr()?)?;
            Some(instr::Stmt::Let { name, value })
        }
        AstStmt::Assign(a) => {
            let name = a.name()?.text().to_string();
            let value = lower_expr(&a.expr()?)?;
            Some(instr::Stmt::Assign { name, value })
        }
        AstStmt::Return(r) => {
            let expr = r
                .expr()
                .and_then(|e| lower_expr(&e))
                .unwrap_or(instr::Expr::Unit);
            Some(instr::Stmt::Return(expr))
        }
        AstStmt::FnDecl(f) => {
            let def = lower_fn_decl(f)?;
            Some(instr::Stmt::FnDecl(Arc::new(def)))
        }
        AstStmt::Expr(e) => {
            let expr = lower_expr(e)?;
            Some(instr::Stmt::Expr(expr))
        }
        AstStmt::CompoundUpdate(_) | AstStmt::FieldAssign(_) => None,
    }
}

fn lower_expr(expr: &AstExpr) -> Option<instr::Expr> {
    match expr {
        AstExpr::Literal(lit) => lower_literal(lit),
        AstExpr::NameRef(name_ref) => {
            let name = name_ref.token()?.text().to_string();
            Some(instr::Expr::Var(name))
        }
        AstExpr::Binary(bin) => lower_binary(bin),
        AstExpr::Prefix(pre) => lower_prefix(pre),
        AstExpr::Block(block) => Some(instr::Expr::Block(lower_block(block))),
        AstExpr::If(if_expr) => lower_if(if_expr),
        AstExpr::Call(call) => lower_call(call),
        AstExpr::Field(field) => lower_field(field),
        AstExpr::StructLiteral(lit) => lower_struct_literal(lit),
        AstExpr::Closure(_)
        | AstExpr::PartialApp(_)
        | AstExpr::Index(_)
        | AstExpr::Match(_)
        | AstExpr::Array(_)
        | AstExpr::Map(_)
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

fn lower_binary(bin: &BinaryExpr) -> Option<instr::Expr> {
    let lhs = lower_expr(&bin.lhs()?)?;
    let rhs = lower_expr(&bin.rhs()?)?;

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

fn lower_prefix(pre: &PrefixExpr) -> Option<instr::Expr> {
    let operand = lower_expr(&pre.expr()?)?;
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

fn lower_if(if_expr: &IfExpr) -> Option<instr::Expr> {
    let cond = lower_expr(&if_expr.condition()?)?;
    let then_expr = if_expr.then_branch()?;
    let then_branch = match then_expr {
        AstExpr::Block(b) => lower_block(&b),
        other => instr::Block {
            stmts: vec![instr::Stmt::Expr(lower_expr(&other)?)],
        },
    };

    let else_branch = match if_expr.else_branch() {
        Some(AstExpr::Block(b)) => lower_block(&b),
        Some(other) => instr::Block {
            stmts: vec![instr::Stmt::Expr(lower_expr(&other)?)],
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

fn lower_call(call: &CallExpr) -> Option<instr::Expr> {
    let callee = call.callee()?;
    let func = lower_expr(&callee)?;

    let args: Vec<instr::Expr> = call
        .args()
        .filter_map(|arg| arg.expr())
        .filter_map(|expr| lower_expr(&expr))
        .collect();

    Some(instr::Expr::Call {
        func: Box::new(func),
        args,
    })
}

fn lower_field(field: &FieldExpr) -> Option<instr::Expr> {
    let receiver = lower_expr(&field.receiver()?)?;
    let field_name = field.field_name()?.text().to_string();
    Some(instr::Expr::Field {
        receiver: Box::new(receiver),
        field: field_name,
    })
}

fn lower_struct_literal(lit: &StructLiteralExpr) -> Option<instr::Expr> {
    let name = lit.name()?.text().to_string();
    let fields: Vec<(String, instr::Expr)> = lit
        .fields()
        .filter_map(|f: StructLiteralField| {
            let field_name = f.name()?.text().to_string();
            let value = lower_expr(&f.expr()?)?;
            Some((field_name, value))
        })
        .collect();

    Some(instr::Expr::StructLit { name, fields })
}
