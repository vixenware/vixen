//! Lowering pass: CST (rowan) → instruction tree (Send-safe).
//!
//! Walks the rowan AST once and produces owned `Expr`/`Stmt`/`FnDef`
//! structures that the evaluator can work with across threads.

use std::sync::Arc;

use vx_parser::{
    ArrayExpr, ArrayItem as AstArrayItem, AstNode, BinaryExpr, BlockExpr, CallExpr, ClosureExpr,
    Expr as AstExpr, FailExpr, FieldExpr, FnDecl, IfExpr, InterpolatedStringExpr, InvokeExpr,
    LiteralExpr, MapExpr, Parameter, PartialAppExpr, PathLiteralExpr, Pattern as AstPattern,
    PrefixExpr, RecordExpr, SourceFile, Stmt as AstStmt, StringPart, StructLiteralExpr,
    StructLiteralField, SyntaxKind, TupleExpr, WhereExpr,
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
    let mut ctx = LowerCtx::default();
    let mut functions = Vec::new();
    let mut top_level = Vec::new();

    for stmt in source.statements() {
        if let AstStmt::FnDecl(f) = &stmt
            && let Some(def) = lower_fn_decl(f)
        {
            let def = Arc::new(def);
            functions.push(Arc::clone(&def));
            top_level.push(instr::Stmt::FnDecl(def));
            continue;
        }

        if let Some(stmt) = lower_stmt_with_ctx(&stmt, &mut ctx) {
            top_level.push(stmt);
        }
    }

    instr::Module {
        functions,
        top_level: instr::Block { stmts: top_level },
    }
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
            let value = lower_expr_with_ctx(&l.expr()?, ctx)?;
            if let Some(pattern) = l.pattern() {
                let pattern = lower_pattern(&pattern)?;
                return match pattern {
                    instr::Pattern::Binding(name) => Some(instr::Stmt::Let { name, value }),
                    pattern => Some(instr::Stmt::LetPattern { pattern, value }),
                };
            }

            let name = l.name()?.text().to_string();
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
        AstStmt::StructDecl(_)
        | AstStmt::EnumDecl(_)
        | AstStmt::NewtypeDecl(_)
        | AstStmt::TypeAliasDecl(_)
        | AstStmt::CompoundUpdate(_)
        | AstStmt::FieldAssign(_) => None,
    }
}

fn lower_expr_with_ctx(expr: &AstExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    match expr {
        AstExpr::Literal(lit) => lower_literal(lit),
        AstExpr::InterpolatedString(string) => lower_interpolated_string(string, ctx),
        AstExpr::PathLiteral(path) => lower_path_literal(path, ctx),
        AstExpr::NameRef(name_ref) => {
            let name = name_ref.token()?.text().to_string();
            Some(instr::Expr::Var(name))
        }
        AstExpr::Binary(bin) => lower_binary(bin, ctx),
        AstExpr::Prefix(pre) => lower_prefix(pre, ctx),
        AstExpr::Block(block) => Some(instr::Expr::Block(lower_block_with_ctx(block, ctx))),
        AstExpr::If(if_expr) => lower_if(if_expr, ctx),
        AstExpr::Tuple(tuple) => lower_tuple(tuple, ctx),
        AstExpr::Paren(paren) => paren
            .expr()
            .and_then(|expr| lower_expr_with_ctx(&expr, ctx)),
        AstExpr::Call(call) => lower_call(call, ctx),
        AstExpr::Where(where_expr) => lower_where(where_expr, ctx),
        AstExpr::Invoke(invoke) => lower_invoke(invoke, ctx),
        AstExpr::Record(record) => lower_record(record, ctx),
        AstExpr::Field(field) => lower_field(field, ctx),
        AstExpr::Fail(fail) => lower_fail(fail, ctx),
        AstExpr::StructLiteral(lit) => lower_struct_literal(lit, ctx),
        AstExpr::Array(array) => lower_array(array, ctx),
        AstExpr::Map(map) => lower_map(map, ctx),
        AstExpr::Closure(closure) => lower_closure(closure, ctx),
        AstExpr::Fallible(fallible) => Some(instr::Expr::Fallible(Box::new(lower_expr_with_ctx(
            &fallible.expr()?,
            ctx,
        )?))),
        AstExpr::PartialApp(partial) => lower_partial(partial, ctx),
        AstExpr::Index(_)
        | AstExpr::Match(_)
        | AstExpr::Range(_)
        | AstExpr::TypeOp(_)
        | AstExpr::With(_) => None,
    }
}

fn lower_pattern(pattern: &AstPattern) -> Option<instr::Pattern> {
    match pattern {
        AstPattern::Binding(binding) => {
            Some(instr::Pattern::Binding(binding.name()?.text().to_string()))
        }
        AstPattern::Struct(pattern) => {
            let mut fields = Vec::new();
            for field in pattern.fields() {
                let name = field.name()?.text().to_string();
                let nested = field.pattern().as_ref().and_then(lower_pattern);
                fields.push((name, nested));
            }
            Some(instr::Pattern::Struct(fields))
        }
        AstPattern::Paren(pattern) => lower_pattern(&pattern.inner()?),
        _ => None,
    }
}

fn lower_tuple(tuple: &TupleExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let items = tuple.items().collect::<Vec<_>>();
    if items.is_empty() {
        return Some(instr::Expr::Unit);
    }

    for item in items {
        let _ = lower_expr_with_ctx(&item, ctx)?;
    }
    None
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
        SyntaxKind::LOWER_IDENT => Some(instr::Expr::LitString(token.text().to_string())),
        _ => None,
    }
}

fn lower_interpolated_string(
    expr: &InterpolatedStringExpr,
    ctx: &mut LowerCtx,
) -> Option<instr::Expr> {
    let parts = lower_template_parts(expr.parts(), ctx)?;
    Some(instr::Expr::StringTemplate(parts))
}

fn lower_path_literal(expr: &PathLiteralExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let text = expr.syntax().text().to_string();
    let text = text.trim();
    if let Some(inner) = text
        .strip_prefix("@(")
        .and_then(|inner| inner.strip_suffix(')'))
    {
        let parts = lower_path_template(inner, ctx)?;
        return Some(instr::Expr::PathTemplate(parts));
    }

    if let Some(inner) = text.strip_prefix('@') {
        return Some(instr::Expr::PathTemplate(vec![instr::TemplatePart::Text(
            inner.to_string(),
        )]));
    }

    if let Some(literal) = expr.literal() {
        let token = literal.token()?;
        if token.kind() != SyntaxKind::STRING {
            return None;
        }
        let text = token.text();
        let inner = &text[1..text.len() - 1];
        return Some(instr::Expr::PathTemplate(vec![instr::TemplatePart::Text(
            unescape_string(inner),
        )]));
    }

    let interpolated = expr.interpolated()?;
    let parts = lower_template_parts(interpolated.parts(), ctx)?;
    Some(instr::Expr::PathTemplate(parts))
}

fn lower_template_parts(
    parts: impl Iterator<Item = StringPart>,
    ctx: &mut LowerCtx,
) -> Option<Vec<instr::TemplatePart>> {
    let mut lowered = Vec::new();
    for part in parts {
        match part {
            StringPart::Text(token) => {
                lowered.push(instr::TemplatePart::Text(unescape_string(token.text())))
            }
            StringPart::Interpolation(interpolation) => lowered.push(instr::TemplatePart::Expr(
                lower_expr_with_ctx(&interpolation.expr()?, ctx)?,
            )),
        }
    }
    Some(lowered)
}

fn lower_path_template(text: &str, ctx: &mut LowerCtx) -> Option<Vec<instr::TemplatePart>> {
    let mut parts = Vec::new();
    let mut cursor = 0usize;

    while let Some(start_offset) = text[cursor..].find('{') {
        let start = cursor + start_offset;
        let end = find_matching_brace(text, start)?;
        if start > cursor {
            parts.push(instr::TemplatePart::Text(text[cursor..start].to_string()));
        }
        let expr_text = text[start + 1..end].trim();
        parts.push(instr::TemplatePart::Expr(lower_embedded_expr(
            expr_text, ctx,
        )?));
        cursor = end + 1;
    }

    if cursor < text.len() {
        parts.push(instr::TemplatePart::Text(text[cursor..].to_string()));
    }

    if parts.is_empty() {
        parts.push(instr::TemplatePart::Text(String::new()));
    }

    Some(parts)
}

fn find_matching_brace(text: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in text[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn lower_embedded_expr(text: &str, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let source = format!("{text};");
    let parse = vx_parser::parse(&source);
    if !parse.errors().is_empty() {
        return None;
    }
    let file = SourceFile::cast_root(parse.syntax());
    let stmt = file.statements().next()?;
    let AstStmt::Expr(expr) = stmt else {
        return None;
    };
    lower_expr_with_ctx(&expr, ctx)
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
    if bin
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .any(|token| token.kind() == SyntaxKind::PIPE_GT)
    {
        return lower_pipe(bin, ctx);
    }

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

fn lower_pipe(bin: &BinaryExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let lhs = lower_expr_with_ctx(&bin.lhs()?, ctx)?;
    let rhs = lower_expr_with_ctx(&bin.rhs()?, ctx)?;

    if contains_pipe_placeholder(&rhs) {
        Some(substitute_pipe_placeholder(rhs, &lhs))
    } else {
        Some(instr::Expr::Call {
            func: Box::new(rhs),
            args: vec![lhs],
            kwargs: vec![],
            kwargs_expr: None,
        })
    }
}

fn contains_pipe_placeholder(expr: &instr::Expr) -> bool {
    match expr {
        instr::Expr::Var(name) => name == "_",
        instr::Expr::Binary { left, right, .. } => {
            contains_pipe_placeholder(left) || contains_pipe_placeholder(right)
        }
        instr::Expr::Unary { operand, .. } => contains_pipe_placeholder(operand),
        instr::Expr::Block(block) => block.stmts.iter().any(stmt_contains_pipe_placeholder),
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            contains_pipe_placeholder(cond)
                || then_branch.stmts.iter().any(stmt_contains_pipe_placeholder)
                || else_branch.stmts.iter().any(stmt_contains_pipe_placeholder)
        }
        instr::Expr::Closure { .. } => false,
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
        }
        | instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
        } => {
            contains_pipe_placeholder(func)
                || args.iter().any(contains_pipe_placeholder)
                || kwargs
                    .iter()
                    .any(|(_, expr)| contains_pipe_placeholder(expr))
                || kwargs_expr
                    .as_deref()
                    .is_some_and(contains_pipe_placeholder)
        }
        instr::Expr::StructLit { fields, .. } => fields
            .iter()
            .any(|(_, expr)| contains_pipe_placeholder(expr)),
        instr::Expr::Record(items) => items.iter().any(|item| match item {
            instr::RecordItem::Field { value, .. } => contains_pipe_placeholder(value),
            instr::RecordItem::Spread(expr) => contains_pipe_placeholder(expr),
        }),
        instr::Expr::Array(items) => items.iter().any(|item| match item {
            instr::ArrayItem::Expr(expr) | instr::ArrayItem::Spread(expr) => {
                contains_pipe_placeholder(expr)
            }
        }),
        instr::Expr::Map(entries) => entries
            .iter()
            .any(|(key, value)| contains_pipe_placeholder(key) || contains_pipe_placeholder(value)),
        instr::Expr::Field { receiver, .. } => contains_pipe_placeholder(receiver),
        instr::Expr::Fail(expr) | instr::Expr::Fallible(expr) => contains_pipe_placeholder(expr),
        instr::Expr::Tuple(items) => items.iter().any(contains_pipe_placeholder),
        instr::Expr::LitInt(_)
        | instr::Expr::LitFloat(_)
        | instr::Expr::LitBool(_)
        | instr::Expr::LitString(_)
        | instr::Expr::StringTemplate(_)
        | instr::Expr::PathTemplate(_)
        | instr::Expr::Unit => false,
    }
}

fn stmt_contains_pipe_placeholder(stmt: &instr::Stmt) -> bool {
    match stmt {
        instr::Stmt::Let { value, .. }
        | instr::Stmt::Assign { value, .. }
        | instr::Stmt::Expr(value)
        | instr::Stmt::Return(value)
        | instr::Stmt::LetPattern { value, .. } => contains_pipe_placeholder(value),
        instr::Stmt::FnDecl(_) => false,
    }
}

fn substitute_pipe_placeholder(expr: instr::Expr, replacement: &instr::Expr) -> instr::Expr {
    match expr {
        instr::Expr::Var(name) if name == "_" => replacement.clone(),
        instr::Expr::Binary { op, left, right } => instr::Expr::Binary {
            op,
            left: Box::new(substitute_pipe_placeholder(*left, replacement)),
            right: Box::new(substitute_pipe_placeholder(*right, replacement)),
        },
        instr::Expr::Unary { op, operand } => instr::Expr::Unary {
            op,
            operand: Box::new(substitute_pipe_placeholder(*operand, replacement)),
        },
        instr::Expr::Block(block) => instr::Expr::Block(instr::Block {
            stmts: block
                .stmts
                .into_iter()
                .map(|stmt| substitute_pipe_placeholder_in_stmt(stmt, replacement))
                .collect(),
        }),
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
        } => instr::Expr::If {
            cond: Box::new(substitute_pipe_placeholder(*cond, replacement)),
            then_branch: instr::Block {
                stmts: then_branch
                    .stmts
                    .into_iter()
                    .map(|stmt| substitute_pipe_placeholder_in_stmt(stmt, replacement))
                    .collect(),
            },
            else_branch: instr::Block {
                stmts: else_branch
                    .stmts
                    .into_iter()
                    .map(|stmt| substitute_pipe_placeholder_in_stmt(stmt, replacement))
                    .collect(),
            },
        },
        instr::Expr::Closure { closure_id, def } => instr::Expr::Closure { closure_id, def },
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
        } => instr::Expr::Call {
            func: Box::new(substitute_pipe_placeholder(*func, replacement)),
            args: args
                .into_iter()
                .map(|expr| substitute_pipe_placeholder(expr, replacement))
                .collect(),
            kwargs: kwargs
                .into_iter()
                .map(|(name, expr)| (name, substitute_pipe_placeholder(expr, replacement)))
                .collect(),
            kwargs_expr: kwargs_expr
                .map(|expr| Box::new(substitute_pipe_placeholder(*expr, replacement))),
        },
        instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
        } => instr::Expr::PartialCall {
            func: Box::new(substitute_pipe_placeholder(*func, replacement)),
            args: args
                .into_iter()
                .map(|expr| substitute_pipe_placeholder(expr, replacement))
                .collect(),
            kwargs: kwargs
                .into_iter()
                .map(|(name, expr)| (name, substitute_pipe_placeholder(expr, replacement)))
                .collect(),
            kwargs_expr: kwargs_expr
                .map(|expr| Box::new(substitute_pipe_placeholder(*expr, replacement))),
        },
        instr::Expr::StructLit { name, fields } => instr::Expr::StructLit {
            name,
            fields: fields
                .into_iter()
                .map(|(field, expr)| (field, substitute_pipe_placeholder(expr, replacement)))
                .collect(),
        },
        instr::Expr::Record(items) => instr::Expr::Record(
            items
                .into_iter()
                .map(|item| match item {
                    instr::RecordItem::Field { name, value } => instr::RecordItem::Field {
                        name,
                        value: substitute_pipe_placeholder(value, replacement),
                    },
                    instr::RecordItem::Spread(expr) => {
                        instr::RecordItem::Spread(substitute_pipe_placeholder(expr, replacement))
                    }
                })
                .collect(),
        ),
        instr::Expr::Array(items) => instr::Expr::Array(
            items
                .into_iter()
                .map(|item| match item {
                    instr::ArrayItem::Expr(expr) => {
                        instr::ArrayItem::Expr(substitute_pipe_placeholder(expr, replacement))
                    }
                    instr::ArrayItem::Spread(expr) => {
                        instr::ArrayItem::Spread(substitute_pipe_placeholder(expr, replacement))
                    }
                })
                .collect(),
        ),
        instr::Expr::Map(entries) => instr::Expr::Map(
            entries
                .into_iter()
                .map(|(key, value)| {
                    (
                        substitute_pipe_placeholder(key, replacement),
                        substitute_pipe_placeholder(value, replacement),
                    )
                })
                .collect(),
        ),
        instr::Expr::Field { receiver, field } => instr::Expr::Field {
            receiver: Box::new(substitute_pipe_placeholder(*receiver, replacement)),
            field,
        },
        instr::Expr::Fail(expr) => {
            instr::Expr::Fail(Box::new(substitute_pipe_placeholder(*expr, replacement)))
        }
        instr::Expr::Fallible(expr) => {
            instr::Expr::Fallible(Box::new(substitute_pipe_placeholder(*expr, replacement)))
        }
        instr::Expr::Tuple(items) => instr::Expr::Tuple(
            items
                .into_iter()
                .map(|expr| substitute_pipe_placeholder(expr, replacement))
                .collect(),
        ),
        other => other,
    }
}

fn substitute_pipe_placeholder_in_stmt(
    stmt: instr::Stmt,
    replacement: &instr::Expr,
) -> instr::Stmt {
    match stmt {
        instr::Stmt::Let { name, value } => instr::Stmt::Let {
            name,
            value: substitute_pipe_placeholder(value, replacement),
        },
        instr::Stmt::LetPattern { pattern, value } => instr::Stmt::LetPattern {
            pattern,
            value: substitute_pipe_placeholder(value, replacement),
        },
        instr::Stmt::Assign { name, value } => instr::Stmt::Assign {
            name,
            value: substitute_pipe_placeholder(value, replacement),
        },
        instr::Stmt::Expr(expr) => {
            instr::Stmt::Expr(substitute_pipe_placeholder(expr, replacement))
        }
        instr::Stmt::Return(expr) => {
            instr::Stmt::Return(substitute_pipe_placeholder(expr, replacement))
        }
        other => other,
    }
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

    let mut args = Vec::new();
    for arg in call.args() {
        args.push(lower_expr_with_ctx(&arg.expr()?, ctx)?);
    }

    Some(instr::Expr::Call {
        func: Box::new(func),
        args,
        kwargs: Vec::new(),
        kwargs_expr: None,
    })
}

fn lower_where(where_expr: &WhereExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let kwargs_expr = lower_expr_with_ctx(&where_expr.keywords()?, ctx)?;

    match where_expr.callee()? {
        AstExpr::Call(call) => {
            let func = lower_expr_with_ctx(&call.callee()?, ctx)?;
            let mut args = Vec::new();
            for arg in call.args() {
                args.push(lower_expr_with_ctx(&arg.expr()?, ctx)?);
            }
            Some(instr::Expr::Call {
                func: Box::new(func),
                args,
                kwargs: Vec::new(),
                kwargs_expr: Some(Box::new(kwargs_expr)),
            })
        }
        callee => {
            let func = lower_expr_with_ctx(&callee, ctx)?;
            Some(instr::Expr::Call {
                func: Box::new(func),
                args: Vec::new(),
                kwargs: Vec::new(),
                kwargs_expr: Some(Box::new(kwargs_expr)),
            })
        }
    }
}

fn lower_invoke(invoke: &InvokeExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let func = lower_expr_with_ctx(&invoke.callee()?, ctx)?;
    Some(instr::Expr::Call {
        func: Box::new(func),
        args: Vec::new(),
        kwargs: Vec::new(),
        kwargs_expr: None,
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

fn lower_record(record: &RecordExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let mut items = Vec::new();
    for entry in record.fields() {
        if let Some(spread) = entry.spread() {
            items.push(instr::RecordItem::Spread(lower_expr_with_ctx(
                &spread.expr()?,
                ctx,
            )?));
            continue;
        }

        let name = entry.name()?.text().to_string();
        let value = match entry.value() {
            Some(expr) => lower_expr_with_ctx(&expr, ctx)?,
            None => instr::Expr::Var(name.clone()),
        };
        items.push(instr::RecordItem::Field { name, value });
    }
    Some(instr::Expr::Record(items))
}

fn lower_struct_literal(lit: &StructLiteralExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let name = lit.name()?.text().to_string();
    let fields: Vec<(String, instr::Expr)> = lit
        .fields()
        .filter_map(|f: StructLiteralField| {
            let field_name = f.name()?.text().to_string();
            let value = match f.expr() {
                Some(expr) => lower_expr_with_ctx(&expr, ctx)?,
                None => instr::Expr::Var(field_name.clone()),
            };
            Some((field_name, value))
        })
        .collect();

    Some(instr::Expr::StructLit { name, fields })
}

fn lower_array(array: &ArrayExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let mut items = Vec::new();
    for item in array.items() {
        let item = match item {
            AstArrayItem::Expr(expr) => instr::ArrayItem::Expr(lower_expr_with_ctx(&expr, ctx)?),
            AstArrayItem::Spread(spread) => {
                instr::ArrayItem::Spread(lower_expr_with_ctx(&spread.expr()?, ctx)?)
            }
        };
        items.push(item);
    }
    Some(instr::Expr::Array(items))
}

fn lower_map(map: &MapExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let mut entries = Vec::new();
    for entry in map.entries() {
        let key = lower_expr_with_ctx(&entry.key()?, ctx)?;
        let value = lower_expr_with_ctx(&entry.value()?, ctx)?;
        entries.push((key, value));
    }
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

fn lower_partial(partial: &PartialAppExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let target = partial.callee()?;
    match target {
        AstExpr::Where(where_expr) => lower_partial_where(&where_expr, ctx),
        AstExpr::Call(call) => {
            let func = lower_expr_with_ctx(&call.callee()?, ctx)?;
            let mut args = Vec::new();
            for arg in call.args() {
                args.push(lower_expr_with_ctx(&arg.expr()?, ctx)?);
            }
            Some(instr::Expr::PartialCall {
                func: Box::new(func),
                args,
                kwargs: Vec::new(),
                kwargs_expr: None,
            })
        }
        AstExpr::Invoke(invoke) => Some(instr::Expr::PartialCall {
            func: Box::new(lower_expr_with_ctx(&invoke.callee()?, ctx)?),
            args: Vec::new(),
            kwargs: Vec::new(),
            kwargs_expr: None,
        }),
        other => Some(instr::Expr::PartialCall {
            func: Box::new(lower_expr_with_ctx(&other, ctx)?),
            args: Vec::new(),
            kwargs: Vec::new(),
            kwargs_expr: None,
        }),
    }
}

fn lower_partial_where(where_expr: &WhereExpr, ctx: &mut LowerCtx) -> Option<instr::Expr> {
    let kwargs_expr = lower_expr_with_ctx(&where_expr.keywords()?, ctx)?;
    match where_expr.callee()? {
        AstExpr::Call(call) => {
            let func = lower_expr_with_ctx(&call.callee()?, ctx)?;
            let mut args = Vec::new();
            for arg in call.args() {
                args.push(lower_expr_with_ctx(&arg.expr()?, ctx)?);
            }
            Some(instr::Expr::PartialCall {
                func: Box::new(func),
                args,
                kwargs: Vec::new(),
                kwargs_expr: Some(Box::new(kwargs_expr)),
            })
        }
        callee => Some(instr::Expr::PartialCall {
            func: Box::new(lower_expr_with_ctx(&callee, ctx)?),
            args: Vec::new(),
            kwargs: Vec::new(),
            kwargs_expr: Some(Box::new(kwargs_expr)),
        }),
    }
}
