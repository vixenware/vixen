//! Tiny parser-backed evaluator for early spec fixtures.

use std::collections::BTreeMap;
use std::sync::Arc;

use vx_parser::{
    ArrayExpr, ArrayItem, AstNode, BlockExpr, CallExpr, Expr, FailExpr, FieldExpr, FnDecl, IfExpr,
    Item, LetStmt, LineIndex as ParserLineIndex, LiteralExpr, MapExpr, NameRefExpr, Parameter,
    SourceFile, Stmt, StructLiteralExpr, SyntaxToken, TextRange, TextSize, trimmed_range,
};

use crate::{
    ByteOffset, ColumnIndex, CrossingPolicy, EvalError, EvalErrorKind, EvalEvent, EvalEventKind,
    EvalEventSink, EvalExprKind, EvalNodeId, EvalTrace, LineIndex, NullEvalEventSink, PendingRef,
    ProjectionStep, Provenance, Resolution, ReuseClass, RuntimeInputKind, SourcePoint, SourceSpan,
    StructValue, Value, ValueMeta, ValueRep,
};

pub trait RuntimeHooks: Send + Sync {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCallOutcome {
    Unsupported,
    Ready(Value),
    Pending { description: Arc<str> },
}

struct NoRuntime;

impl RuntimeHooks for NoRuntime {
    fn call(
        &self,
        _place: &str,
        _callee: &str,
        _args: &[Value],
        _source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        Ok(RuntimeCallOutcome::Unsupported)
    }
}

pub fn eval_source_entry(source: &str, entry: &str) -> Result<Resolution, EvalError> {
    eval_source_entry_with_source_id(source, "<memory>", entry)
}

pub fn eval_source_entry_with_source_id_traced(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
) -> Result<(Resolution, EvalTrace), EvalError> {
    eval_source_entry_with_args_with_source_id_traced(source, source_id, entry, &[])
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
    let mut sink = NullEvalEventSink;
    eval_source_entry_with_args_with_source_id_with_sink(source, source_id, entry, args, &mut sink)
}

pub fn eval_source_entry_with_args_with_source_id_traced(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
) -> Result<(Resolution, EvalTrace), EvalError> {
    let mut trace = EvalTrace::default();
    let resolution = eval_source_entry_with_args_with_source_id_with_sink(
        source, source_id, entry, args, &mut trace,
    )
    .unwrap_or_else(Resolution::Poison);
    Ok((resolution, trace))
}

pub fn eval_source_entry_at_place_with_args_with_source_id_traced(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<(Resolution, EvalTrace), EvalError> {
    let mut trace = EvalTrace::default();
    let resolution = eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, source_id, entry, args, runtime, place, &mut trace,
    )
    .unwrap_or_else(Resolution::Poison);
    Ok((resolution, trace))
}

pub fn eval_source_entry_at_place(
    source: &str,
    entry: &str,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source,
        "<memory>",
        entry,
        &[],
        runtime,
        place,
        &mut sink,
    )
}

pub fn eval_source_entry_at_place_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source,
        source_id,
        entry,
        &[],
        runtime,
        place,
        &mut sink,
    )
}

pub fn eval_source_entry_at_place_with_args(
    source: &str,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, "<memory>", entry, args, runtime, place, &mut sink,
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
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, source_id, entry, args, runtime, place, &mut sink,
    )
}

pub fn eval_source_entry_with_args_with_source_id_with_sink(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    sink: &mut impl EvalEventSink,
) -> Result<Resolution, EvalError> {
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, source_id, entry, args, &NoRuntime, "local", sink,
    )
}

pub fn eval_source_entry_at_place_with_args_with_source_id_with_sink(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
    sink: &mut impl EvalEventSink,
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
    let mut state = EvalState::new(&source, runtime, place, sink);
    let mut env = bind_arguments(&mut state, &parameters, args)?;
    Ok(eval_expr(&mut state, &body, &mut env)?.into_resolution())
}

struct SourceContext {
    source_id: String,
    line_index: ParserLineIndex,
}

struct EvalState<'a, Runtime, Sink> {
    source: &'a SourceContext,
    runtime: &'a Runtime,
    place: &'a str,
    sink: &'a mut Sink,
    scheduler: EvalScheduler,
    stack: Vec<EvalNodeId>,
}

#[derive(Default)]
struct EvalScheduler {
    next_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvalStep {
    Ready(Value),
    Pending(PendingRef),
}

impl<'a, Runtime: RuntimeHooks, Sink: EvalEventSink> EvalState<'a, Runtime, Sink> {
    fn new(
        source: &'a SourceContext,
        runtime: &'a Runtime,
        place: &'a str,
        sink: &'a mut Sink,
    ) -> Self {
        Self {
            source,
            runtime,
            place,
            sink,
            scheduler: EvalScheduler { next_id: 1 },
            stack: Vec::new(),
        }
    }

    fn alloc_node_id(&mut self) -> EvalNodeId {
        self.scheduler.alloc_node_id()
    }

    fn alloc_pending(&mut self, description: impl Into<Arc<str>>) -> PendingRef {
        self.scheduler.alloc_pending(description)
    }

    fn current_parent(&self) -> Option<EvalNodeId> {
        self.stack.last().copied()
    }

    fn emit(
        &mut self,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        source: Option<SourceSpan>,
        kind: EvalEventKind,
    ) {
        self.sink.push(EvalEvent {
            node_id,
            parent_id,
            place: Arc::from(self.place),
            source,
            kind,
        });
    }

    fn emit_current_dependency(&mut self, dependency: EvalNodeId, source: Option<SourceSpan>) {
        if let Some(node_id) = self.current_parent() {
            let parent_id = self.stack.iter().rev().nth(1).copied();
            self.emit(
                node_id,
                parent_id,
                source,
                EvalEventKind::DependOn { dependency },
            );
        }
    }
}

impl EvalScheduler {
    fn alloc_node_id(&mut self) -> EvalNodeId {
        let node_id = EvalNodeId(self.next_id);
        self.next_id += 1;
        node_id
    }

    fn alloc_pending(&mut self, description: impl Into<Arc<str>>) -> PendingRef {
        let pending_id = self.alloc_node_id();
        PendingRef::new(pending_id.0, description)
    }
}

impl EvalStep {
    fn into_resolution(self) -> Resolution {
        match self {
            Self::Ready(value) => Resolution::Ready(value),
            Self::Pending(pending) => Resolution::Pending(pending),
        }
    }
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
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
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
        let node_id = state.alloc_node_id();
        let parent_id = state.current_parent();
        state.emit(
            node_id,
            parent_id,
            Some(state.source.span_for_token(&name)),
            EvalEventKind::BindArgument {
                label: Arc::from(label.as_str()),
            },
        );
        let bound = arg.clone().with_provenance(Provenance::runtime_input(
            state.source.span_for_token(&name),
            RuntimeInputKind::Argument,
            label.clone(),
        ));
        env.insert(label, bound);
    }
    Ok(env)
}

fn eval_expr(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state.alloc_node_id();
    eval_expr_with_node_id(state, node_id, expr, env)
}

fn eval_expr_with_node_id(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let parent_id = state.current_parent();
    state.emit(
        node_id,
        parent_id,
        Some(state.source.span_for_node(expr)),
        EvalEventKind::EnterExpr {
            kind: expr_kind(expr),
        },
    );
    state.stack.push(node_id);

    let result = match expr {
        Expr::Block(block) => eval_block(state, block, env),
        Expr::Literal(literal) => eval_literal(state.source, literal).map(EvalStep::Ready),
        Expr::Paren(paren) => match paren.expr() {
            Some(inner) => eval_child_expr(state, &inner, env),
            None => Ok(EvalStep::Ready(Value::unit())),
        },
        Expr::NameRef(name_ref) => eval_name_ref(name_ref, env).map(EvalStep::Ready),
        Expr::Call(call) => eval_call(state, call, env),
        Expr::Array(array) => eval_array(state, array, env),
        Expr::Map(map) => eval_map(state, map, env),
        Expr::Fail(fail) => eval_fail(state, fail, env),
        Expr::If(if_expr) => eval_if(state, if_expr, env),
        Expr::StructLiteral(struct_literal) => eval_struct_literal(state, struct_literal, env),
        Expr::Field(field) => eval_field(state, field, env),
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported expression: {other:?}"),
        )),
    };

    let popped = state.stack.pop();
    debug_assert_eq!(popped, Some(node_id));
    match &result {
        Ok(EvalStep::Ready(value)) => state.emit(
            node_id,
            parent_id,
            None,
            EvalEventKind::ExitReady {
                reuse: value.meta.reuse.clone(),
                crossing: value.meta.crossing.clone(),
            },
        ),
        Ok(EvalStep::Pending(pending)) => state.emit(
            node_id,
            parent_id,
            None,
            EvalEventKind::ExitPending {
                pending: pending.clone(),
            },
        ),
        Err(error) => state.emit(
            node_id,
            parent_id,
            None,
            EvalEventKind::ExitPoison {
                kind: error.kind,
                message: error.message.clone(),
            },
        ),
    }

    result
}

fn eval_child_expr(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let child_id = state.alloc_node_id();
    state.emit_current_dependency(child_id, Some(state.source.span_for_node(expr)));
    eval_expr_with_node_id(state, child_id, expr, env)
}

fn eval_call(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    call: &CallExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
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
    let mut args = Vec::new();
    for arg in call.positional_args() {
        match eval_child_expr(state, &arg, env)? {
            EvalStep::Ready(value) => args.push(value),
            EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
        }
    }
    let call_source = state.source.span_for_node(call);
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("call expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    state.emit(
        node_id,
        parent_id,
        Some(call_source.clone()),
        EvalEventKind::RuntimeCallStarted {
            callee: Arc::from(callee_name.as_str()),
            argc: args.len() as u32,
        },
    );
    match state
        .runtime
        .call(state.place, &callee_name, &args, &call_source)?
    {
        RuntimeCallOutcome::Ready(value) => {
            state.emit(
                node_id,
                parent_id,
                Some(call_source),
                EvalEventKind::RuntimeCallFinishedReady {
                    reuse: value.meta.reuse.clone(),
                    crossing: value.meta.crossing.clone(),
                },
            );
            Ok(EvalStep::Ready(value))
        }
        RuntimeCallOutcome::Pending { description } => {
            let pending = state.alloc_pending(description);
            state.emit(
                node_id,
                parent_id,
                Some(call_source.clone()),
                EvalEventKind::RegisterPending {
                    pending: pending.clone(),
                },
            );
            state.emit(
                node_id,
                parent_id,
                Some(call_source),
                EvalEventKind::RuntimeCallFinishedPending {
                    pending: pending.clone(),
                },
            );
            Ok(EvalStep::Pending(pending))
        }
        RuntimeCallOutcome::Unsupported => {
            state.emit(
                node_id,
                parent_id,
                Some(call_source),
                EvalEventKind::RuntimeCallFinishedPoison {
                    kind: EvalErrorKind::Internal,
                    message: Arc::from(format!("call target {callee_name} is not supported yet")),
                },
            );
            Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("call target {callee_name} is not supported yet"),
            ))
        }
    }
}

fn eval_let(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    let_stmt: &LetStmt,
    env: &mut BTreeMap<String, Value>,
) -> Result<Option<PendingRef>, EvalError> {
    let name_token = let_stmt
        .name()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "let statement has no name"))?;
    let name = name_token.text().to_string();
    let value = match eval_child_expr(
        state,
        &let_stmt
            .expr()
            .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "let statement has no value"))?,
        env,
    )? {
        EvalStep::Ready(value) => value.with_provenance(Provenance::binding(
            state.source.span_for_token(&name_token),
            name.clone(),
        )),
        EvalStep::Pending(pending) => return Ok(Some(pending)),
    };
    env.insert(name, value);
    Ok(None)
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
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    struct_literal: &StructLiteralExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
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
        let value = match eval_child_expr(
            state,
            &field.expr().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "struct field has no value")
            })?,
            env,
        )? {
            EvalStep::Ready(value) => value,
            EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
        };
        fields.push((name, value));
    }
    let provenance = Provenance::literal(state.source.span_for_node(struct_literal));
    let meta = aggregate_child_meta(provenance, fields.iter().map(|(_, value)| value));
    Ok(EvalStep::Ready(
        Value::structure(type_name, fields).with_meta(meta),
    ))
}

fn eval_block(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    block: &BlockExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let mut scope = env.clone();
    let mut last = Value::unit();
    for statement in block.statements() {
        match statement {
            Stmt::Let(let_stmt) => {
                if let Some(pending) = eval_let(state, &let_stmt, &mut scope)? {
                    return Ok(EvalStep::Pending(pending));
                }
            }
            Stmt::Expr(expr) => match eval_child_expr(state, &expr, &mut scope)? {
                EvalStep::Ready(value) => last = value,
                EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
            },
            other => {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!("unsupported block statement: {other:?}"),
                ));
            }
        }
    }
    Ok(EvalStep::Ready(last))
}

fn eval_array(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    array: &ArrayExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let mut items = Vec::new();
    for item in array.items() {
        match item {
            ArrayItem::Expr(expr) => match eval_child_expr(state, &expr, env)? {
                EvalStep::Ready(value) => items.push(value),
                EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
            },
            ArrayItem::Spread(_) => {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "array spread is not supported yet",
                ));
            }
        }
    }
    let provenance = Provenance::literal(state.source.span_for_node(array));
    let meta = aggregate_child_meta(provenance, items.iter());
    Ok(EvalStep::Ready(Value::array(items).with_meta(meta)))
}

fn eval_map(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    map: &MapExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let mut entries = Vec::new();
    for entry in map.entries() {
        let key_expr = entry.key().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "map entry has no explicit key")
        })?;
        let key = match eval_child_expr(state, &key_expr, env)? {
            EvalStep::Ready(value) => value,
            EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
        };
        let value_expr = entry
            .value()
            .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "map entry has no value"))?;
        let value = match eval_child_expr(state, &value_expr, env)? {
            EvalStep::Ready(value) => value,
            EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
        };
        entries.push((key, value));
    }
    let provenance = Provenance::literal(state.source.span_for_node(map));
    let meta = aggregate_child_meta(
        provenance,
        entries.iter().flat_map(|(key, value)| [key, value]),
    );
    Ok(EvalStep::Ready(Value::map(entries).with_meta(meta)))
}

fn eval_fail(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    fail: &FailExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let message = match eval_child_expr(
        state,
        &fail.message().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "fail expression has no message")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
    };
    let ValueRep::String(message) = message.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "fail message must evaluate to String",
        ));
    };
    Err(EvalError::new(EvalErrorKind::UserFail, message.to_string()))
}

fn eval_if(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    if_expr: &IfExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let condition = match eval_child_expr(
        state,
        &if_expr.condition().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "if expression has no condition")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
    };
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
    eval_child_expr(state, &branch, env)
}

fn eval_field(
    state: &mut EvalState<'_, impl RuntimeHooks, impl EvalEventSink>,
    field: &FieldExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let receiver = match eval_child_expr(
        state,
        &field.receiver().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "field expression has no receiver")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Pending(pending) => return Ok(EvalStep::Pending(pending)),
    };
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
    Ok(EvalStep::Ready(
        value.project(ProjectionStep::Field(field_name.into())),
    ))
}

fn expr_kind(expr: &Expr) -> EvalExprKind {
    match expr {
        Expr::Block(_) => EvalExprKind::Block,
        Expr::Literal(_) => EvalExprKind::Literal,
        Expr::Paren(_) => EvalExprKind::Paren,
        Expr::NameRef(_) => EvalExprKind::NameRef,
        Expr::Call(_) => EvalExprKind::Call,
        Expr::Array(_) => EvalExprKind::Array,
        Expr::Map(_) => EvalExprKind::Map,
        Expr::Fail(_) => EvalExprKind::Fail,
        Expr::If(_) => EvalExprKind::If,
        Expr::StructLiteral(_) => EvalExprKind::StructLiteral,
        Expr::Field(_) => EvalExprKind::Field,
        _ => EvalExprKind::Unsupported,
    }
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
