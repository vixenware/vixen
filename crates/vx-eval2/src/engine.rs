//! Tiny parser-backed evaluator for early spec fixtures.

use std::collections::BTreeMap;
use std::sync::Arc;

use vx_parser::{
    ArrayItem, AstNode, CallExpr, Expr, FnDecl, Item, LetStmt, LineIndex as ParserLineIndex,
    LiteralExpr, NameRefExpr, Parameter, SourceFile, Stmt, StructLiteralExpr, SyntaxToken,
    TextRange, TextSize, trimmed_range,
};

use crate::{
    ByteOffset, ColumnIndex, CrossingPolicy, EvalError, EvalErrorKind, LineIndex, ProjectionStep,
    Provenance, Resolution, ReuseClass, RuntimeInputKind, SourcePoint, SourceSpan, StructValue,
    Value, ValueMeta, ValueRep,
};

pub trait RuntimeHooks {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<Option<Value>, EvalError>;
}

struct NoRuntime;

impl RuntimeHooks for NoRuntime {
    fn call(
        &self,
        _place: &str,
        _callee: &str,
        _args: &[Value],
        _source: &SourceSpan,
    ) -> Result<Option<Value>, EvalError> {
        Ok(None)
    }
}

pub fn eval_source_entry(source: &str, entry: &str) -> Result<Resolution, EvalError> {
    eval_source_entry_with_source_id(source, "<memory>", entry)
}

pub fn eval_source_entry_with_args(
    source: &str,
    entry: &str,
    args: &[Value],
) -> Result<Resolution, EvalError> {
    eval_source_entry_with_args_with_source_id(source, "<memory>", entry, args)
}

pub fn eval_source_entry_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
) -> Result<Resolution, EvalError> {
    eval_source_entry_with_args_with_source_id(source, source_id, entry, &[])
}

pub fn eval_source_entry_with_args_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
) -> Result<Resolution, EvalError> {
    eval_source_entry_at_place_with_args_with_source_id(
        source, source_id, entry, args, &NoRuntime, "local",
    )
}

pub fn eval_source_entry_at_place(
    source: &str,
    entry: &str,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    eval_source_entry_at_place_with_args_with_source_id(
        source,
        "<memory>",
        entry,
        &[],
        runtime,
        place,
    )
}

pub fn eval_source_entry_at_place_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    eval_source_entry_at_place_with_args_with_source_id(
        source,
        source_id,
        entry,
        &[],
        runtime,
        place,
    )
}

pub fn eval_source_entry_at_place_with_args(
    source: &str,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    eval_source_entry_at_place_with_args_with_source_id(
        source, "<memory>", entry, args, runtime, place,
    )
}

pub fn eval_source_entry_at_place_with_args_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let parse = vx_parser::parse(source);
    if !parse.errors().is_empty() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("parse failed: {:?}", parse.errors()),
        ));
    }

    let source = SourceContext::new(source_id, source);
    let root = SourceFile::cast_root(parse.syntax());
    let function = find_function(&root, entry)?;
    if function.keyword_parameters().next().is_some() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("functions with keyword parameters are not supported yet: {entry}"),
        ));
    }
    let parameters = function.parameters().collect::<Vec<_>>();
    if parameters.len() != args.len() {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "function {entry} expects {} positional argument(s), got {}",
                parameters.len(),
                args.len()
            ),
        ));
    }
    let body = function.body_expr().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            format!("function {entry} has no body expression"),
        )
    })?;
    let mut env = bind_arguments(&source, &parameters, args)?;
    Ok(Resolution::Ready(eval_expr(
        &source, &body, &mut env, runtime, place,
    )?))
}

struct SourceContext {
    source_id: String,
    line_index: ParserLineIndex,
}

impl SourceContext {
    fn new(source_id: impl Into<String>, source: &str) -> Self {
        Self {
            source_id: source_id.into(),
            line_index: ParserLineIndex::new(source),
        }
    }

    fn span_for_node<T: AstNode>(&self, node: &T) -> SourceSpan {
        self.span_for_range(trimmed_range(node.syntax()))
    }

    fn span_for_token(&self, token: &SyntaxToken) -> SourceSpan {
        self.span_for_range(token.text_range())
    }

    fn span_for_range(&self, range: TextRange) -> SourceSpan {
        SourceSpan::new(
            self.source_id.clone(),
            self.point_for_offset(range.start()),
            self.point_for_offset(range.end()),
        )
    }

    fn point_for_offset(&self, offset: TextSize) -> SourcePoint {
        let position = self.line_index.position(offset);
        SourcePoint::new(
            ByteOffset(u32::from(offset)),
            LineIndex(position.line),
            ColumnIndex(position.col),
        )
    }
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

fn bind_arguments(
    source: &SourceContext,
    parameters: &[Parameter],
    args: &[Value],
) -> Result<BTreeMap<String, Value>, EvalError> {
    let mut env = BTreeMap::new();
    for (parameter, arg) in parameters.iter().zip(args.iter()) {
        let name = parameter.name().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "function parameter has no name token",
            )
        })?;
        let label = name.text().to_string();
        let bound = arg.clone().with_provenance(Provenance::runtime_input(
            source.span_for_token(&name),
            RuntimeInputKind::Argument,
            label.clone(),
        ));
        env.insert(label, bound);
    }
    Ok(env)
}

fn eval_expr(
    source: &SourceContext,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Value, EvalError> {
    match expr {
        Expr::Block(block) => {
            let mut scope = env.clone();
            let mut last = Value::unit();
            for statement in block.statements() {
                match statement {
                    Stmt::Let(let_stmt) => eval_let(source, &let_stmt, &mut scope, runtime, place)?,
                    Stmt::Expr(expr) => {
                        last = eval_expr(source, &expr, &mut scope, runtime, place)?;
                    }
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
        Expr::Literal(literal) => eval_literal(source, literal),
        Expr::Paren(paren) => match paren.expr() {
            Some(inner) => eval_expr(source, &inner, env, runtime, place),
            None => Ok(Value::unit()),
        },
        Expr::NameRef(name_ref) => eval_name_ref(name_ref, env),
        Expr::Call(call) => eval_call(source, call, env, runtime, place),
        Expr::Array(array) => {
            let mut items = Vec::new();
            for item in array.items() {
                match item {
                    ArrayItem::Expr(expr) => {
                        items.push(eval_expr(source, &expr, env, runtime, place)?)
                    }
                    ArrayItem::Spread(_) => {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "array spread is not supported yet",
                        ));
                    }
                }
            }
            let provenance = Provenance::literal(source.span_for_node(array));
            let meta = aggregate_child_meta(provenance, items.iter());
            Ok(Value::array(items).with_meta(meta))
        }
        Expr::Map(map) => {
            let mut entries = Vec::new();
            for entry in map.entries() {
                let key_expr = entry.key().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "map entry has no explicit key")
                })?;
                let key = eval_expr(source, &key_expr, env, runtime, place)?;
                let value_expr = entry.value().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "map entry has no value")
                })?;
                let value = eval_expr(source, &value_expr, env, runtime, place)?;
                entries.push((key, value));
            }
            let provenance = Provenance::literal(source.span_for_node(map));
            let meta = aggregate_child_meta(
                provenance,
                entries.iter().flat_map(|(key, value)| [key, value]),
            );
            Ok(Value::map(entries).with_meta(meta))
        }
        Expr::Fail(fail) => {
            let message = eval_expr(
                source,
                &fail.message().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "fail expression has no message")
                })?,
                env,
                runtime,
                place,
            )?;
            let ValueRep::String(message) = message.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "fail message must evaluate to String",
                ));
            };
            Err(EvalError::new(EvalErrorKind::UserFail, message.to_string()))
        }
        Expr::If(if_expr) => {
            let condition = eval_expr(
                source,
                &if_expr.condition().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "if expression has no condition")
                })?,
                env,
                runtime,
                place,
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
            eval_expr(source, &branch, env, runtime, place)
        }
        Expr::StructLiteral(struct_literal) => {
            eval_struct_literal(source, struct_literal, env, runtime, place)
        }
        Expr::Field(field) => {
            let receiver = eval_expr(
                source,
                &field.receiver().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "field expression has no receiver")
                })?,
                env,
                runtime,
                place,
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

fn eval_call(
    source: &SourceContext,
    call: &CallExpr,
    env: &mut BTreeMap<String, Value>,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Value, EvalError> {
    let callee = call
        .callee()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "call expression has no callee"))?;
    let Expr::NameRef(name_ref) = callee else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "only direct name calls are supported yet",
        ));
    };
    let callee_name = name_ref
        .token()
        .map(|token| token.text().to_string())
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "callee name has no token"))?;
    let args = call
        .positional_args()
        .into_iter()
        .map(|arg| eval_expr(source, &arg, env, runtime, place))
        .collect::<Result<Vec<_>, _>>()?;
    let call_source = source.span_for_node(call);
    if let Some(value) = runtime.call(place, &callee_name, &args, &call_source)? {
        return Ok(value);
    }
    Err(EvalError::new(
        EvalErrorKind::Internal,
        format!("call target {callee_name} is not supported yet"),
    ))
}

fn eval_let(
    source: &SourceContext,
    let_stmt: &LetStmt,
    env: &mut BTreeMap<String, Value>,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<(), EvalError> {
    let name_token = let_stmt
        .name()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "let statement has no name"))?;
    let name = name_token.text().to_string();
    let value = eval_expr(
        source,
        &let_stmt
            .expr()
            .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "let statement has no value"))?,
        env,
        runtime,
        place,
    )?
    .with_provenance(Provenance::binding(
        source.span_for_token(&name_token),
        name.clone(),
    ));
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
    source: &SourceContext,
    struct_literal: &StructLiteralExpr,
    env: &mut BTreeMap<String, Value>,
    runtime: &impl RuntimeHooks,
    place: &str,
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
            source,
            &field.expr().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "struct field has no value")
            })?,
            env,
            runtime,
            place,
        )?;
        fields.push((name, value));
    }
    let provenance = Provenance::literal(source.span_for_node(struct_literal));
    let meta = aggregate_child_meta(provenance, fields.iter().map(|(_, value)| value));
    Ok(Value::structure(type_name, fields).with_meta(meta))
}

fn aggregate_child_meta<'a>(
    provenance: Provenance,
    children: impl IntoIterator<Item = &'a Value>,
) -> ValueMeta {
    let mut meta = ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue);
    let mut saw_blocked = false;

    for child in children {
        meta.reuse = join_reuse(meta.reuse.clone(), child.meta.reuse.clone());
        meta = meta.with_claims(child.meta.claims.iter().cloned());
        saw_blocked |= child.meta.crossing == CrossingPolicy::Blocked;
    }

    if saw_blocked {
        meta.crossing = CrossingPolicy::Blocked;
    }

    meta
}

fn join_reuse(left: ReuseClass, right: ReuseClass) -> ReuseClass {
    match (left, right) {
        (ReuseClass::Live, _) | (_, ReuseClass::Live) => ReuseClass::Live,
        (ReuseClass::Observed, _) | (_, ReuseClass::Observed) => ReuseClass::Observed,
        (ReuseClass::Stable, ReuseClass::Stable) => ReuseClass::Stable,
    }
}

fn eval_literal(source: &SourceContext, literal: &LiteralExpr) -> Result<Value, EvalError> {
    let token = literal
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "literal has no token"))?;
    let literal_source = source.span_for_node(literal);
    match token.kind() {
        vx_parser::SyntaxKind::INT => {
            let value = token.text().parse::<u64>().map_err(|source| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("invalid integer literal {}: {source}", token.text()),
                )
            })?;
            Ok(Value::u64(value).with_provenance(Provenance::literal(literal_source)))
        }
        vx_parser::SyntaxKind::TRUE_KW => {
            Ok(Value::bool(true).with_provenance(Provenance::literal(literal_source)))
        }
        vx_parser::SyntaxKind::FALSE_KW => {
            Ok(Value::bool(false).with_provenance(Provenance::literal(literal_source)))
        }
        vx_parser::SyntaxKind::STRING => {
            let text = token.text();
            Ok(Value::string(&text[1..text.len() - 1])
                .with_provenance(Provenance::literal(literal_source)))
        }
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported literal token: {other:?}"),
        )),
    }
}
