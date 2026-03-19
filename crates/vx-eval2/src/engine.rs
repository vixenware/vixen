//! Tiny parser-backed evaluator for early spec fixtures.

use std::collections::BTreeMap;
use std::sync::Arc;

use vx_parser::{
    Expr, FnDecl, Item, LetStmt, LiteralExpr, NameRefExpr, SourceFile, Stmt, StructLiteralExpr,
};

use crate::{
    EvalError, EvalErrorKind, ProjectionStep, Provenance, Resolution, StructValue, Value, ValueRep,
};

pub fn eval_source_entry(source: &str, entry: &str) -> Result<Resolution, EvalError> {
    let parse = vx_parser::parse(source);
    if !parse.errors().is_empty() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("parse failed: {:?}", parse.errors()),
        ));
    }

    let root = SourceFile::cast_root(parse.syntax());
    let function = find_function(&root, entry)?;
    if function.parameters().next().is_some() || function.keyword_parameters().next().is_some() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("functions with parameters are not supported yet: {entry}"),
        ));
    }
    let body = function.body_expr().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            format!("function {entry} has no body expression"),
        )
    })?;
    let mut env = BTreeMap::new();
    Ok(Resolution::Ready(eval_expr(&body, &mut env)?))
}

fn find_function(root: &SourceFile, entry: &str) -> Result<FnDecl, EvalError> {
    root.items()
        .find_map(|item| match item {
            Item::FnDecl(function)
                if function
                    .name()
                    .as_ref()
                    .is_some_and(|name| name.text() == entry) =>
            {
                Some(function)
            }
            _ => None,
        })
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("function {entry} was not found"),
            )
        })
}

fn eval_expr(expr: &Expr, env: &mut BTreeMap<String, Value>) -> Result<Value, EvalError> {
    match expr {
        Expr::Block(block) => {
            let mut scope = env.clone();
            let mut last = Value::unit();
            for statement in block.statements() {
                match statement {
                    Stmt::Let(let_stmt) => eval_let(&let_stmt, &mut scope)?,
                    Stmt::Expr(expr) => last = eval_expr(&expr, &mut scope)?,
                    other => {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            format!("unsupported block statement: {other:?}"),
                        ));
                    }
                }
            }
            Ok(last)
        }
        Expr::Literal(literal) => eval_literal(literal),
        Expr::Paren(paren) => match paren.expr() {
            Some(inner) => eval_expr(&inner, env),
            None => Ok(Value::unit()),
        },
        Expr::NameRef(name_ref) => eval_name_ref(name_ref, env),
        Expr::If(if_expr) => {
            let condition = eval_expr(
                &if_expr.condition().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "if expression has no condition")
                })?,
                env,
            )?;
            let ValueRep::Bool(value) = condition.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "if condition must evaluate to Bool",
                ));
            };
            let branch = if value {
                if_expr.then_branch()
            } else {
                if_expr.else_branch()
            }
            .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "if branch is missing"))?;
            eval_expr(&branch, env)
        }
        Expr::StructLiteral(struct_literal) => eval_struct_literal(struct_literal, env),
        Expr::Field(field) => {
            let receiver = eval_expr(
                &field.receiver().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "field expression has no receiver")
                })?,
                env,
            )?;
            let field_name = field
                .field_name()
                .map(|name| name.text().to_string())
                .ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "field expression has no field name",
                    )
                })?;
            let ValueRep::Struct(StructValue { fields, .. }) = &receiver.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("cannot project field {field_name} from non-struct value"),
                ));
            };
            let value = fields
                .iter()
                .find_map(|(name, value)| (name.as_ref() == field_name).then(|| value.clone()))
                .ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::TypeError,
                        format!("struct field {field_name} was not found"),
                    )
                })?;
            Ok(value.project(ProjectionStep::Field(field_name.into())))
        }
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported expression: {other:?}"),
        )),
    }
}

fn eval_let(let_stmt: &LetStmt, env: &mut BTreeMap<String, Value>) -> Result<(), EvalError> {
    let name = let_stmt
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "let statement has no name"))?;
    let value = eval_expr(
        &let_stmt
            .expr()
            .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "let statement has no value"))?,
        env,
    )?;
    env.insert(name, value);
    Ok(())
}

fn eval_name_ref(
    name_ref: &NameRefExpr,
    env: &BTreeMap<String, Value>,
) -> Result<Value, EvalError> {
    let name = name_ref
        .token()
        .map(|token| token.text().to_string())
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "name reference has no token"))?;
    env.get(&name).cloned().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            format!("name {name} is not bound in this scope"),
        )
    })
}

fn eval_struct_literal(
    struct_literal: &StructLiteralExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<Value, EvalError> {
    let type_name = struct_literal
        .name()
        .map(|name| Arc::<str>::from(name.text()))
        .ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "struct literal has no type name")
        })?;
    let mut fields = Vec::new();
    for field in struct_literal.fields() {
        let name = Arc::<str>::from(
            field
                .name()
                .map(|name| name.text().to_string())
                .ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "struct field has no name")
                })?,
        );
        let value = eval_expr(
            &field.expr().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "struct field has no value")
            })?,
            env,
        )?;
        fields.push((name, value));
    }
    Ok(Value::structure(type_name, fields).with_provenance(Provenance::literal()))
}

fn eval_literal(literal: &LiteralExpr) -> Result<Value, EvalError> {
    let token = literal
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "literal has no token"))?;
    match token.kind() {
        vx_parser::SyntaxKind::INT => {
            let value = token.text().parse::<u64>().map_err(|source| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("invalid integer literal {}: {source}", token.text()),
                )
            })?;
            Ok(Value::u64(value).with_provenance(Provenance::literal()))
        }
        vx_parser::SyntaxKind::TRUE_KW => {
            Ok(Value::bool(true).with_provenance(Provenance::literal()))
        }
        vx_parser::SyntaxKind::FALSE_KW => {
            Ok(Value::bool(false).with_provenance(Provenance::literal()))
        }
        vx_parser::SyntaxKind::STRING => {
            let text = token.text();
            Ok(Value::string(&text[1..text.len() - 1]).with_provenance(Provenance::literal()))
        }
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported literal token: {other:?}"),
        )),
    }
}
