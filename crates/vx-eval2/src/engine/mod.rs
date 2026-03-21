//! Tiny parser-backed evaluator for early spec fixtures.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque, btree_map::Entry};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, mpsc};
use std::thread;

use vx_parser::{
    ArrayExpr, ArrayItem, AstNode, BinaryExpr, BlockExpr, CallExpr, ClosureExpr, Expr, FailExpr,
    FieldExpr, IfExpr, LiteralExpr, MapExpr, NameRefExpr, Stmt, StructLiteralExpr, SyntaxKind,
};
use vx_resolve::SourceId;
use vx_types::Ty;

use crate::{
    CrossingPolicy, EvalError, EvalErrorKind, EvalEvent, EvalEventKind, EvalEventSink,
    EvalExprKind, EvalNodeId, EvalTrace, FunctionRef, NullEvalEventSink, PendingRef,
    ProjectionStep, Provenance, Resolution, ReuseClass, RuntimeInputKind, SlotAction, SourceSpan,
    StructValue, Value, ValueMeta, ValueRep,
    decode::{StructuredFormat, parse_blob_as_value},
    program::{CompiledParameter, CompiledProgram, EvalProgram, ProgramNodeHandle},
};

mod graph;
pub use graph::*;

mod session;
pub use session::*;

mod state;
use state::*;

mod work;
use work::*;

pub trait RuntimeHooks: Send + Sync {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError>;

    fn prepare_slot(
        &self,
        _place: &str,
        _slot_id: &str,
        _witness: &Value,
        _action: &SlotAction<Value>,
    ) -> Result<RuntimeSlotPrepare, EvalError> {
        Err(EvalError::new(
            EvalErrorKind::Internal,
            "slot operations are not supported by this runtime",
        ))
    }

    fn store_slot(
        &self,
        _place: &str,
        _slot_id: &str,
        _witness: &Value,
        _action: &SlotAction<Value>,
        _resolved: &Value,
    ) -> Result<RuntimeReady, EvalError> {
        Err(EvalError::new(
            EvalErrorKind::Internal,
            "slot operations are not supported by this runtime",
        ))
    }
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
    let mut scheduler = EvalScheduler::default();
    eval_source_entry_at_place_with_args_with_source_id_with_scheduler_and_sink(
        source,
        source_id,
        entry,
        args,
        runtime,
        place,
        &mut scheduler,
        sink,
    )
}

fn eval_source_entry_at_place_with_args_with_source_id_with_scheduler_and_sink(
    source_text: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
    scheduler: &mut EvalScheduler,
    sink: &mut impl EvalEventSink,
) -> Result<Resolution, EvalError> {
    let program = CompiledProgram::new(source_id, source_text);
    let mut state = EvalState::new(
        &program,
        program.entry_source(),
        EvalBranchContext::default(),
        runtime,
        place,
        sink,
        scheduler,
        None,
        false,
    );
    match evaluate_entry_once(&mut state, entry, args)? {
        EvalStep::Ready(value) => Ok(Resolution::Ready(value)),
        EvalStep::Suspended(suspension) => Ok(Resolution::Pending(suspension.pending)),
    }
}

struct PreparedEntry {
    source: SourceId,
    body: Expr,
    env: EvalEnv,
}

fn function_ref_from_compiled(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    name: Arc<str>,
    function: crate::program::CompiledFunction,
    captures: Arc<[(Arc<str>, Value)]>,
    self_name: Option<Arc<str>>,
) -> FunctionRef {
    FunctionRef::new(
        state.program.source_id(function.source),
        name,
        function.parameters,
        function.has_keyword_parameters,
        function.body,
        captures,
        self_name,
    )
}

fn captures_from_scope(scope: &EvalEnv) -> Arc<[(Arc<str>, Value)]> {
    Arc::from(
        scope
            .iter()
            .map(|(name, value)| (Arc::<str>::from(name.as_str()), value.clone()))
            .collect::<Vec<_>>(),
    )
}

fn compile_closure_parameters(
    source: SourceId,
    closure: &ClosureExpr,
) -> Result<Arc<[CompiledParameter]>, EvalError> {
    closure
        .parameters()
        .map(|parameter| {
            let name = parameter.name().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "closure parameter has no binding name",
                )
            })?;
            Ok(CompiledParameter {
                label: Arc::from(name.text()),
                name: ProgramNodeHandle::for_token(source, &name),
            })
        })
        .collect::<Result<Vec<_>, EvalError>>()
        .map(Arc::from)
}

fn bind_named_function_decl(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    function: &vx_parser::FnDecl,
    scope: &EvalEnv,
) -> Result<(String, SourceSpan, Value), EvalError> {
    let name_token = function.name().ok_or_else(|| {
        EvalError::new(EvalErrorKind::Internal, "function declaration has no name")
    })?;
    let name = name_token.text().to_string();
    let name_source = state.span_for_token(&name_token);
    let Some((compiled_name, compiled)) = state
        .program
        .compile_function_decl(state.current_source(), function)?
    else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "function declaration could not be compiled",
        ));
    };
    let function = function_ref_from_compiled(
        state,
        compiled_name.clone(),
        compiled,
        captures_from_scope(scope),
        Some(compiled_name.clone()),
    );
    let value = Value::function(function)
        .with_provenance(Provenance::binding(name_source.clone(), name.clone()));
    Ok((name, name_source, value))
}

fn eval_closure(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    closure: &ClosureExpr,
    env: &EvalEnv,
) -> Result<Value, EvalError> {
    let body = closure
        .body()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "closure expression has no body"))?;
    let closure_source = state.span_for_node(closure);
    let function = FunctionRef::new(
        state.program.source_id(state.current_source()),
        Arc::from("<closure>"),
        compile_closure_parameters(state.current_source(), closure)?,
        false,
        Some(ProgramNodeHandle::for_node(state.current_source(), &body)),
        captures_from_scope(env),
        None,
    );
    Ok(Value::function(function).with_provenance(Provenance::literal(closure_source)))
}

fn prepare_entry(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    entry: &str,
    args: &[Value],
) -> Result<PreparedEntry, EvalError> {
    let function = state.program.find_function(entry)?;
    if function.has_keyword_parameters {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("functions with keyword parameters are not supported yet: {entry}"),
        ));
    }
    if function.parameters.len() != args.len() {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "function {entry} expects {} positional argument(s), got {}",
                function.parameters.len(),
                args.len()
            ),
        ));
    }
    let body = function.body.ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            format!("function {entry} has no body expression"),
        )
    })?;
    let env = bind_arguments(state, &function.parameters, args, None)?;
    Ok(PreparedEntry {
        source: body.source(),
        body: state.program.resolve_expr(body)?,
        env,
    })
}

fn prepare_function_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    function: &FunctionRef,
    args: &[Value],
    parent_id: Option<EvalNodeId>,
) -> Result<Option<PreparedEntry>, EvalError> {
    let Some(body) = function.body else {
        return Ok(None);
    };
    if function.has_keyword_parameters {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!(
                "functions with keyword parameters are not supported yet: {}",
                function.name
            ),
        ));
    }
    if function.parameters.len() != args.len() {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "function {} expects {} positional argument(s), got {}",
                function.name,
                function.parameters.len(),
                args.len()
            ),
        ));
    }
    let mut env = Arc::new(function.capture_env());
    if let Some(self_name) = &function.self_name {
        env_mut(&mut env).insert(self_name.to_string(), Value::function(function.clone()));
    }
    bind_arguments_into_env(state, &mut env, &function.parameters, args, parent_id)?;
    Ok(Some(PreparedEntry {
        source: body.source(),
        body: state.program.resolve_expr(body)?,
        env,
    }))
}

fn keyed_context_value(key: Value, joined: Value) -> Value {
    let fields = vec![(Arc::from("key"), key), (Arc::from("joined"), joined)];
    let meta = aggregate_child_meta(
        Provenance::unknown(SourceSpan::synthetic("<keyed-context>")),
        fields.iter().map(|(_, value)| value),
    );
    Value::structure("KeyedContext", fields).with_meta(meta)
}

fn prepare_keyed_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
) -> Result<PreparedEntry, EvalError> {
    let (resolver, key, joined) = {
        let memo = state.scheduler.keyed_node(node_id).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("keyed node {:?} is not registered", node_id),
            )
        })?;
        (memo.resolver.clone(), memo.key.clone(), memo.joined.clone())
    };
    let resolver_context = keyed_context_value(key, joined);
    let Some(prepared) =
        prepare_function_call(state, &resolver, &[resolver_context], Some(node_id))?
    else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "keyed() resolver must be a bodyful Vixen function",
        ));
    };
    Ok(prepared)
}

fn evaluate_entry_once(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    entry: &str,
    args: &[Value],
) -> Result<EvalStep, EvalError> {
    let prepared = prepare_entry(state, entry, args)?;
    let mut env = prepared.env;
    state.with_source(prepared.source, |state| {
        eval_expr(state, &prepared.body, &mut env)
    })
}

fn bind_arguments(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    parameters: &[CompiledParameter],
    args: &[Value],
    parent_id: Option<EvalNodeId>,
) -> Result<EvalEnv, EvalError> {
    let mut env = empty_env();
    bind_arguments_into_env(state, &mut env, parameters, args, parent_id)?;
    Ok(env)
}

fn bind_arguments_into_env(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    env: &mut EvalEnv,
    parameters: &[CompiledParameter],
    args: &[Value],
    parent_id: Option<EvalNodeId>,
) -> Result<(), EvalError> {
    for (parameter, arg) in parameters.iter().zip(args.iter()) {
        let label = parameter.label.to_string();
        let name_source = state.program.span_for_handle(parameter.name);
        let node_id = state.alloc_node_id();
        state.emit(
            node_id,
            parent_id,
            Some(name_source.clone()),
            EvalEventKind::BindArgument {
                label: Arc::from(label.as_str()),
            },
        );
        state.scheduler.record_node(EvalNodeRecord {
            node_id,
            parent_id,
            kind: EvalNodeKind::ArgumentBinding {
                label: Arc::from(label.as_str()),
            },
            source: Some(name_source.clone()),
        });
        let bound = arg.clone().with_provenance(Provenance::runtime_input(
            name_source,
            RuntimeInputKind::Argument,
            label.clone(),
        ));
        env_mut(env).insert(label, bound);
    }
    Ok(())
}

fn eval_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state.alloc_node_id();
    eval_expr_with_node_id(state, node_id, expr, env)
}

fn eval_expr_with_node_id(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    expr: &Expr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let parent_id = state.current_parent();
    let kind = expr_kind(expr);
    let source = state.span_for_node(expr);
    state.scheduler.record_node(EvalNodeRecord {
        node_id,
        parent_id,
        kind: EvalNodeKind::Expr { kind },
        source: Some(source.clone()),
    });
    state.emit(
        node_id,
        parent_id,
        Some(source),
        EvalEventKind::EnterExpr { kind },
    );
    state.stack.push(node_id);

    let result = match expr {
        Expr::Block(block) => eval_block(state, block, env),
        Expr::Literal(literal) => eval_literal(state, literal).map(EvalStep::Ready),
        Expr::Paren(paren) => match paren.expr() {
            Some(inner) => match eval_child_expr(state, &inner, env)? {
                EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
                EvalStep::Suspended(suspension) => Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::FinishNode { node_id, parent_id },
                )),
            },
            None => Ok(EvalStep::Ready(Value::unit())),
        },
        Expr::Closure(closure) => eval_closure(state, closure, env).map(EvalStep::Ready),
        Expr::NameRef(name_ref) => eval_name_ref(state, name_ref, env).map(EvalStep::Ready),
        Expr::Call(call) => eval_call(state, call, env),
        Expr::Array(array) => eval_array(state, array, env),
        Expr::Map(map) => eval_map(state, map, env),
        Expr::Fail(fail) => eval_fail(state, fail, env),
        Expr::If(if_expr) => eval_if(state, if_expr, env),
        Expr::Binary(binary) => eval_binary(state, binary, env),
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
        Ok(EvalStep::Ready(value)) => {
            record_ready_step(state, node_id, parent_id, value);
        }
        Ok(EvalStep::Suspended(suspension)) => {
            record_pending_step(state, node_id, parent_id, &suspension.pending);
        }
        Err(error) => {
            record_poison_step(state, node_id, parent_id, error);
        }
    }

    result
}

fn eval_child_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    if !state.queue_child_work {
        let child_id = state.alloc_node_id();
        state.emit_current_dependency(child_id, Some(state.span_for_node(expr)));
        return eval_expr_with_node_id(state, child_id, expr, env);
    }
    let child_id = queue_child_expr(state, expr, env);
    let resume_node = state
        .current_parent()
        .expect("child expression should have an active parent node");
    Ok(wait_on_child_dependency(
        state,
        resume_node,
        child_id,
        Vec::new(),
    ))
}

fn eval_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    call: &CallExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let callee = call
        .callee()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "call expression has no callee"))?;
    let call_source = state.span_for_node(call);
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("call expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let type_args = Arc::<[Ty]>::from(
        call.type_args()
            .into_iter()
            .flat_map(|args| args.types())
            .map(|ty| state.program.resolve_type_annotation(&ty))
            .collect::<Vec<_>>(),
    );
    let positional_args = call.positional_args().into_iter().collect::<Vec<_>>();
    if let Some(callee_value) = immediate_call_callee_value(state, &callee, env)? {
        if state.queue_child_work && !positional_args.is_empty() {
            let child_nodes = positional_args
                .iter()
                .map(|arg| queue_child_expr(state, arg, env))
                .collect::<Vec<_>>();
            return Ok(wait_on_child_dependency(
                state,
                node_id,
                child_nodes[0],
                vec![ContinuationFrame::CallJoin {
                    node_id,
                    parent_id,
                    callee: callee_value,
                    call_source,
                    type_args: Arc::clone(&type_args),
                    child_nodes,
                }],
            ));
        }
        return continue_call_from_args(
            state,
            node_id,
            parent_id,
            callee_value,
            call_source,
            Arc::clone(&type_args),
            Vec::new(),
            positional_args
                .iter()
                .map(|arg| state.expr_handle(arg))
                .collect(),
            env.clone(),
        );
    }
    let callee_value = match eval_call_callee(state, &callee, env)? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::CallAfterCallee {
                    node_id,
                    parent_id,
                    call_source,
                    type_args: Arc::clone(&type_args),
                    env: env.clone(),
                    args: Vec::new(),
                    remaining_args: positional_args
                        .iter()
                        .map(|arg| state.expr_handle(arg))
                        .collect(),
                },
            ));
        }
    };
    continue_call_from_args(
        state,
        node_id,
        parent_id,
        callee_value,
        call_source,
        type_args,
        Vec::new(),
        positional_args
            .iter()
            .map(|arg| state.expr_handle(arg))
            .collect(),
        env.clone(),
    )
}

fn immediate_call_callee_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    callee: &Expr,
    env: &EvalEnv,
) -> Result<Option<Value>, EvalError> {
    let Expr::NameRef(name_ref) = callee else {
        return Ok(None);
    };
    let name_token = name_ref
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "callee name has no token"))?;
    let name = name_token.text().to_string();
    if let Some(value) = env.get(&name) {
        return Ok(Some(value.clone()));
    }
    let reference = ProgramNodeHandle::for_token(state.current_source(), &name_token);
    if let Some((resolved_name, function)) =
        state.program.resolve_function_reference(reference, &name)?
    {
        return Ok(Some(Value::function(function_ref_from_compiled(
            state,
            resolved_name.clone(),
            function,
            Arc::from([]),
            Some(resolved_name),
        ))));
    }
    Ok(Some(Value::function(FunctionRef::new(
        state.program.source_id(state.current_source()),
        Arc::from(name.as_str()),
        Arc::from([]),
        false,
        None,
        Arc::from([]),
        Some(Arc::from(name.as_str())),
    ))))
}

fn eval_name_ref(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    name_ref: &NameRefExpr,
    env: &EvalEnv,
) -> Result<Value, EvalError> {
    let name_token = name_ref
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "name reference has no token"))?;
    let name = name_token.text().to_string();
    if let Some(value) = env.get(&name) {
        return Ok(value.clone());
    }
    let reference = ProgramNodeHandle::for_token(state.current_source(), &name_token);
    if let Some((resolved_name, function)) =
        state.program.resolve_function_reference(reference, &name)?
    {
        let name_source = state.span_for_token(&name_token);
        let function = function_ref_from_compiled(
            state,
            resolved_name.clone(),
            function,
            Arc::from([]),
            Some(resolved_name),
        );
        return Ok(
            Value::function(function).with_provenance(Provenance::binding(name_source, name))
        );
    }
    Err(EvalError::new(
        EvalErrorKind::Internal,
        format!("name {name} is not bound in this scope"),
    ))
}

fn eval_call_callee(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    callee: &Expr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    if let Some(value) = immediate_call_callee_value(state, callee, env)? {
        return Ok(EvalStep::Ready(value));
    }
    eval_child_expr(state, callee, env)
}

fn static_namespace_member_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    field: &FieldExpr,
    env: &EvalEnv,
) -> Result<Option<Value>, EvalError> {
    let Some(Expr::NameRef(receiver)) = field.receiver() else {
        return Ok(None);
    };
    let Some(receiver_token) = receiver.token() else {
        return Ok(None);
    };
    if receiver_token.kind() != SyntaxKind::UPPER_IDENT {
        return Ok(None);
    }
    if env.contains_key(receiver_token.text()) {
        return Ok(None);
    }
    let Some(field_name) = field.field_name() else {
        return Ok(None);
    };
    let full_name = format!("{}.{}", receiver_token.text(), field_name.text());
    let function = FunctionRef::new(
        state.program.source_id(state.current_source()),
        Arc::from(full_name.as_str()),
        Arc::from([]),
        false,
        None,
        Arc::from([]),
        None,
    );
    let source = state.span_for_node(field);
    Ok(Some(
        Value::function(function).with_provenance(Provenance::binding(source, full_name)),
    ))
}

fn slot_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Arc<str>, Value, SlotAction<Value>, FunctionRef)>, EvalError> {
    let (slot_id_index, witness_index, action, resolver_index) = match callee_name {
        "slot_current" => (0usize, 1usize, SlotAction::Current, 2usize),
        "slot_refresh" => {
            if args.len() != 4 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "slot_refresh() requires slot id, witness, intent, and resolver",
                ));
            }
            (0usize, 1usize, SlotAction::Refresh(args[2].clone()), 3usize)
        }
        _ => return Ok(None),
    };

    let expected_len = match callee_name {
        "slot_current" => 3,
        "slot_refresh" => 4,
        _ => unreachable!(),
    };
    if args.len() != expected_len {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "{callee_name}() requires {} argument(s), got {}",
                expected_len,
                args.len()
            ),
        ));
    }

    let ValueRep::String(slot_id) = &args[slot_id_index].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee_name}() slot id must evaluate to String"),
        ));
    };
    let ValueRep::Function(resolver) = &args[resolver_index].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee_name}() resolver must evaluate to a function value"),
        ));
    };

    Ok(Some((
        slot_id.clone(),
        args[witness_index].clone(),
        action,
        resolver.clone(),
    )))
}

fn keyed_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Value, Value, FunctionRef)>, EvalError> {
    if callee_name != "keyed" {
        return Ok(None);
    }
    if args.len() != 3 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "keyed() requires key, joined input, and resolver",
        ));
    }
    let ValueRep::Function(resolver) = &args[2].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "keyed() resolver must evaluate to a function value",
        ));
    };
    Ok(Some((args[0].clone(), args[1].clone(), resolver.clone())))
}

fn branch_assumption_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<Arc<str>>, EvalError> {
    if callee_name != "branch_assumption" {
        return Ok(None);
    }
    if args.len() != 1 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "branch_assumption() requires the assumption name",
        ));
    }
    let ValueRep::String(name) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "branch_assumption() name must evaluate to String",
        ));
    };
    Ok(Some(name.clone()))
}

fn with_assumption_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Arc<str>, Value, FunctionRef)>, EvalError> {
    if callee_name != "with_assumption" {
        return Ok(None);
    }
    if args.len() != 3 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() requires name, value, and resolver",
        ));
    }
    let ValueRep::String(name) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() name must evaluate to String",
        ));
    };
    let ValueRep::Function(resolver) = &args[2].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() resolver must evaluate to a function value",
        ));
    };
    Ok(Some((name.clone(), args[1].clone(), resolver.clone())))
}

fn search_assumption_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Arc<str>, Vec<Value>, FunctionRef)>, EvalError> {
    if callee_name != "search_assumption" {
        return Ok(None);
    }
    if args.len() != 3 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() requires name, candidates, and resolver",
        ));
    }
    let ValueRep::String(name) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() name must evaluate to String",
        ));
    };
    let ValueRep::Array(candidates) = &args[1].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() candidates must evaluate to an array",
        ));
    };
    let ValueRep::Function(resolver) = &args[2].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() resolver must evaluate to a function value",
        ));
    };
    Ok(Some((
        name.clone(),
        candidates.iter().cloned().collect(),
        resolver.clone(),
    )))
}

fn ready_builtin_value(value: Value) -> Option<EvalStep> {
    Some(EvalStep::Ready(value))
}

fn continue_array_find_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    items: Vec<Value>,
    predicate: FunctionRef,
    start_index: usize,
) -> Result<EvalStep, EvalError> {
    for index in start_index..items.len() {
        let item = items[index].clone();
        let Some(prepared) =
            prepare_function_call(state, &predicate, &[item.clone()], Some(node_id))?
        else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "array_find() predicate must be a bodyful Vixen function",
            ));
        };
        let mut env = prepared.env;
        let matched = match state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, node_id, &prepared.body, &mut env)
        })? {
            EvalStep::Ready(result) => result,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFindAfterPredicate {
                        node_id,
                        parent_id,
                        items,
                        predicate,
                        index,
                    },
                ));
            }
        };
        match matched.rep {
            ValueRep::Bool(true) => {
                return Ok(EvalStep::Ready(
                    item.project(ProjectionStep::Index(index as u64)),
                ));
            }
            ValueRep::Bool(false) => {}
            _ => {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() predicate must return Bool",
                ));
            }
        }
    }
    Err(EvalError::new(
        EvalErrorKind::TypeError,
        "array_find() found no matching element",
    ))
}

fn continue_array_fold_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    items: Vec<Value>,
    mut accumulator: Value,
    step: FunctionRef,
    start_index: usize,
) -> Result<EvalStep, EvalError> {
    for index in start_index..items.len() {
        let item = items[index].clone();
        let Some(prepared) =
            prepare_function_call(state, &step, &[accumulator.clone(), item], Some(node_id))?
        else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "array_fold() step must be a bodyful Vixen function",
            ));
        };
        let mut env = prepared.env;
        accumulator = match state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, node_id, &prepared.body, &mut env)
        })? {
            EvalStep::Ready(result) => result,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFoldAfterStep {
                        node_id,
                        parent_id,
                        items,
                        step,
                        index,
                    },
                ));
            }
        };
    }
    Ok(EvalStep::Ready(accumulator))
}

fn branch_attempt_value(candidate: Value, error: EvalError) -> Value {
    let conflict = error
        .detail
        .unwrap_or_else(|| Value::string(error.message.as_ref()));
    let fields = vec![
        (Arc::from("value"), candidate),
        (Arc::from("conflict"), conflict),
    ];
    let meta = aggregate_child_meta(
        Provenance::unknown(SourceSpan::synthetic("<branch-attempt>")),
        fields.iter().map(|(_, value)| value),
    );
    Value::structure("BranchAttempt", fields).with_meta(meta)
}

fn branch_conflict_error(name: Arc<str>, attempts: Vec<Value>) -> EvalError {
    let fields = vec![
        (Arc::from("assumption"), Value::string(name)),
        (Arc::from("attempts"), Value::array(attempts)),
    ];
    let meta = aggregate_child_meta(
        Provenance::unknown(SourceSpan::synthetic("<branch-conflict>")),
        fields.iter().map(|(_, value)| value),
    );
    user_fail_error(Value::structure("BranchConflict", fields).with_meta(meta))
}

fn candidate_lists_equal(lhs: &[Value], rhs: &[Value]) -> Result<bool, EvalError> {
    if lhs.len() != rhs.len() {
        return Ok(false);
    }
    lhs.iter()
        .zip(rhs.iter())
        .try_fold(true, |equal, (lhs, rhs)| {
            Ok(equal && values_equal(&lhs.rep, &rhs.rep)?)
        })
}

fn errors_equal_for_learning(lhs: &EvalError, rhs: &EvalError) -> Result<bool, EvalError> {
    if lhs.kind != rhs.kind || lhs.message != rhs.message {
        return Ok(false);
    }
    match (&lhs.detail, &rhs.detail) {
        (Some(lhs), Some(rhs)) => values_equal(&lhs.rep, &rhs.rep),
        (None, None) => Ok(true),
        _ => Ok(false),
    }
}

fn branch_conflict_attempts(error: &EvalError) -> Option<&[Value]> {
    let detail = error.detail.as_ref()?;
    let ValueRep::Struct(struct_value) = &detail.rep else {
        return None;
    };
    if struct_value.type_name.as_ref() != "BranchConflict" {
        return None;
    }
    let attempts_value = struct_value
        .fields
        .iter()
        .find(|(field, _)| field.as_ref() == "attempts")
        .map(|(_, value)| value)?;
    let ValueRep::Array(attempts) = &attempts_value.rep else {
        return None;
    };
    Some(&attempts[..])
}

fn local_branch_conflict_error(
    name: Arc<str>,
    error: &EvalError,
    attempt_offset: usize,
) -> Option<EvalError> {
    if attempt_offset == 0 {
        return Some(error.clone());
    }
    let attempts = branch_conflict_attempts(error)?;
    Some(branch_conflict_error(
        name,
        attempts[attempt_offset..].to_vec(),
    ))
}

fn learn_point_no_good(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    branch_context: &EvalBranchContext,
    assumption: &Arc<str>,
    candidates: &[Value],
    index: usize,
    resolver: &FunctionRef,
    error: &EvalError,
) -> Result<(), EvalError> {
    state.scheduler.record_learned_no_good(LearnedNoGood {
        branch_context: branch_context.clone(),
        assumption: assumption.clone(),
        candidates: Arc::from(candidates.to_vec()),
        start_index: index,
        end_index: index,
        resolver: resolver.clone(),
        error: error.clone(),
    })
}

fn learn_search_no_good(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    branch_context: &EvalBranchContext,
    assumption: &Arc<str>,
    candidates: &[Value],
    resolver: &FunctionRef,
    error: &EvalError,
) -> Result<(), EvalError> {
    state
        .scheduler
        .record_learned_search_no_good(LearnedSearchNoGood {
            branch_context: branch_context.clone(),
            assumption: assumption.clone(),
            candidates: Arc::from(candidates.to_vec()),
            resolver: resolver.clone(),
            error: error.clone(),
        })
}

fn continue_search_assumption_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    name: Arc<str>,
    candidates: Vec<Value>,
    resolver: FunctionRef,
    start_index: usize,
    mut attempts: Vec<Value>,
    root_search_start: usize,
    search_start_index: usize,
) -> Result<EvalStep, EvalError> {
    let prior_branch_context = state.branch_context.clone();
    let local_attempt_offset = search_start_index.saturating_sub(root_search_start);
    let fresh_local_search = attempts.len() == local_attempt_offset;
    let search_candidates = &candidates[search_start_index..];
    if fresh_local_search
        && let Some(error) = state.scheduler.learned_search_no_good(
            &prior_branch_context,
            &name,
            search_candidates,
            &resolver,
        )?
    {
        return Err(error);
    }
    for index in start_index..candidates.len() {
        let candidate = candidates[index].clone();
        if let Some(error) = state.scheduler.learned_no_good(
            &prior_branch_context,
            &name,
            &candidates,
            index,
            &resolver,
        )? {
            attempts.push(branch_attempt_value(candidate, error));
            continue;
        }
        let branch_context = prior_branch_context.with_assumption(name.clone(), candidate.clone());
        let Some(prepared) = prepare_function_call(state, &resolver, &[], Some(node_id))? else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "search_assumption() resolver must be a bodyful Vixen function",
            ));
        };
        let mut env = prepared.env;
        match state.with_branch_context(branch_context, |state| {
            state.with_source(prepared.source, |state| {
                eval_dependent_expr(state, node_id, &prepared.body, &mut env)
            })
        }) {
            Ok(EvalStep::Ready(value)) => return Ok(EvalStep::Ready(value)),
            Ok(EvalStep::Suspended(suspension)) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::SearchAssumptionAfterBranch {
                        node_id,
                        parent_id,
                        name,
                        candidates,
                        resolver,
                        index,
                        attempts,
                        root_search_start,
                        search_start_index,
                        prior_branch_context,
                    },
                ));
            }
            Err(error) if error.kind == EvalErrorKind::UserFail => {
                learn_point_no_good(
                    state,
                    &prior_branch_context,
                    &name,
                    &candidates,
                    index,
                    &resolver,
                    &error,
                )?;
                attempts.push(branch_attempt_value(candidate, error));
            }
            Err(error) => return Err(error),
        }
    }
    let error = branch_conflict_error(name.clone(), attempts);
    if !search_candidates.is_empty() {
        let local_error = local_branch_conflict_error(name.clone(), &error, local_attempt_offset)
            .unwrap_or_else(|| error.clone());
        learn_search_no_good(
            state,
            &prior_branch_context,
            &name,
            search_candidates,
            &resolver,
            &local_error,
        )?;
    }
    Err(error)
}

fn aggregate_map_entries_meta<'a>(
    provenance: Provenance,
    entries: impl IntoIterator<Item = &'a (Value, Value)>,
) -> ValueMeta {
    aggregate_child_meta(
        provenance,
        entries.into_iter().flat_map(|(key, value)| [key, value]),
    )
}

fn aggregate_array_items_meta<'a>(
    provenance: Provenance,
    items: impl IntoIterator<Item = &'a Value>,
) -> ValueMeta {
    aggregate_child_meta(provenance, items)
}

fn map_entry_value(map_provenance: &Provenance, index: usize, key: &Value, value: &Value) -> Value {
    let entry_provenance = map_provenance.project(ProjectionStep::Index(index as u64));
    let entry_fields = vec![
        (Value::string("key"), key.clone()),
        (Value::string("value"), value.clone()),
    ];
    let entry_meta = aggregate_map_entries_meta(entry_provenance, entry_fields.iter());
    Value::map(entry_fields).with_meta(entry_meta)
}

fn continue_builtin_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    callee_name: &str,
    type_args: &[Ty],
    args: &[Value],
) -> Result<Option<EvalStep>, EvalError> {
    match callee_name {
        "Json.parse" | "Toml.parse" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee_name}() requires one Blob argument"),
                ));
            }
            if type_args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee_name}() requires exactly one type argument"),
                ));
            }
            let format = match callee_name {
                "Json.parse" => StructuredFormat::Json,
                "Toml.parse" => StructuredFormat::Toml,
                _ => unreachable!(),
            };
            let value =
                parse_blob_as_value(format, &args[0], &type_args[0], state.program.types())?;
            Ok(ready_builtin_value(value))
        }
        "map_get" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_get() requires map and key",
                ));
            }
            let ValueRep::Map(map) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_get() first argument must evaluate to a map",
                ));
            };
            let key = &args[1];
            let mut selected = None;
            for (entry_key, entry_value) in map.entries.iter() {
                if values_equal(&entry_key.rep, &key.rep)? {
                    selected = Some(entry_value.clone());
                    break;
                }
            }
            let mut value = selected.ok_or_else(|| {
                EvalError::new(EvalErrorKind::TypeError, "map_get() key was not found")
            })?;
            if let ValueRep::String(key) = &key.rep {
                value = value.project(ProjectionStep::MapKey(key.to_string()));
            }
            Ok(ready_builtin_value(value))
        }
        "map_put" => {
            if args.len() != 3 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_put() requires map, key, and value",
                ));
            }
            let ValueRep::Map(map) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_put() first argument must evaluate to a map",
                ));
            };
            let key = &args[1];
            let value = &args[2];
            let mut entries = map.entries.iter().cloned().collect::<Vec<_>>();
            let mut replaced = false;
            for (entry_key, entry_value) in &mut entries {
                if values_equal(&entry_key.rep, &key.rep)? {
                    *entry_value = value.clone();
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                entries.push((key.clone(), value.clone()));
            }
            let meta = aggregate_map_entries_meta(args[0].meta.provenance.clone(), entries.iter());
            Ok(ready_builtin_value(Value::map(entries).with_meta(meta)))
        }
        "map_entries" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_entries() requires map",
                ));
            }
            let ValueRep::Map(map) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_entries() first argument must evaluate to a map",
                ));
            };
            let items = map
                .entries
                .iter()
                .enumerate()
                .map(|(index, (key, value))| {
                    map_entry_value(&args[0].meta.provenance, index, key, value)
                })
                .collect::<Vec<_>>();
            let meta = aggregate_child_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "array_first" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_first() requires array",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_first() first argument must evaluate to an array",
                ));
            };
            let value = items.first().cloned().ok_or_else(|| {
                EvalError::new(EvalErrorKind::TypeError, "array_first() array was empty")
            })?;
            Ok(ready_builtin_value(value.project(ProjectionStep::Index(0))))
        }
        "array_rest" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_rest() requires array",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_rest() first argument must evaluate to an array",
                ));
            };
            let items = items.iter().skip(1).cloned().collect::<Vec<_>>();
            let meta = aggregate_array_items_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "array_push" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_push() requires array and item",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_push() first argument must evaluate to an array",
                ));
            };
            let mut items = items.iter().cloned().collect::<Vec<_>>();
            items.push(args[1].clone());
            let meta = aggregate_array_items_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "array_find" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() requires array and predicate",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() first argument must evaluate to an array",
                ));
            };
            let ValueRep::Function(predicate) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() second argument must evaluate to a function value",
                ));
            };
            continue_array_find_builtin(
                state,
                node_id,
                parent_id,
                items.iter().cloned().collect(),
                predicate.clone(),
                0,
            )
            .map(Some)
        }
        "array_fold" => {
            if args.len() != 3 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_fold() requires array, initial, and step",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_fold() first argument must evaluate to an array",
                ));
            };
            let ValueRep::Function(step) = &args[2].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_fold() third argument must evaluate to a function value",
                ));
            };
            continue_array_fold_builtin(
                state,
                node_id,
                parent_id,
                items.iter().cloned().collect(),
                args[1].clone(),
                step.clone(),
                0,
            )
            .map(Some)
        }
        _ => Ok(None),
    }
}

fn slot_context_value(
    slot_id: Arc<str>,
    witness: Value,
    action: &SlotAction<Value>,
    current: Option<Value>,
) -> Value {
    let (is_refresh, intent) = match action {
        SlotAction::Current => (false, Value::unit()),
        SlotAction::Refresh(intent) => (true, intent.clone()),
    };
    let current_present = current.is_some();
    Value::structure(
        "SlotContext",
        vec![
            (Arc::from("slot_id"), Value::string(slot_id)),
            (Arc::from("witness"), witness),
            (Arc::from("has_current"), Value::bool(current_present)),
            (Arc::from("current"), current.unwrap_or_else(Value::unit)),
            (Arc::from("is_refresh"), Value::bool(is_refresh)),
            (Arc::from("intent"), intent),
        ],
    )
}

fn emit_runtime_ready_events(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    source: &SourceSpan,
    ready: &RuntimeReady,
) {
    for event in &ready.events {
        state.emit(node_id, parent_id, Some(source.clone()), event.clone());
    }
}

fn commit_slot_result(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    call_source: &SourceSpan,
    slot_id: &Arc<str>,
    witness: &Value,
    action: &SlotAction<Value>,
    resolved: &Value,
) -> Result<EvalStep, EvalError> {
    let ready =
        state
            .runtime
            .store_slot(state.place, slot_id.as_ref(), witness, action, resolved)?;
    emit_runtime_ready_events(state, node_id, parent_id, call_source, &ready);
    Ok(EvalStep::Ready(ready.value))
}

fn continue_slot_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    call_source: SourceSpan,
    slot_id: Arc<str>,
    witness: Value,
    action: SlotAction<Value>,
    resolver: FunctionRef,
) -> Result<EvalStep, EvalError> {
    match state
        .runtime
        .prepare_slot(state.place, slot_id.as_ref(), &witness, &action)?
    {
        RuntimeSlotPrepare::Reused(ready) => {
            emit_runtime_ready_events(state, node_id, parent_id, &call_source, &ready);
            Ok(EvalStep::Ready(ready.value))
        }
        RuntimeSlotPrepare::Resolve { current } => {
            let resolver_context =
                slot_context_value(slot_id.clone(), witness.clone(), &action, current);
            let Some(prepared) =
                prepare_function_call(state, &resolver, &[resolver_context], Some(node_id))?
            else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "slot resolver must be a bodyful Vixen function",
                ));
            };
            let mut env = prepared.env;
            match state.with_source(prepared.source, |state| {
                eval_dependent_expr(state, node_id, &prepared.body, &mut env)
            })? {
                EvalStep::Ready(value) => commit_slot_result(
                    state,
                    node_id,
                    parent_id,
                    &call_source,
                    &slot_id,
                    &witness,
                    &action,
                    &value,
                ),
                EvalStep::Suspended(suspension) => Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::SlotAfterResolver {
                        node_id,
                        parent_id,
                        call_source,
                        slot_id,
                        witness,
                        action,
                    },
                )),
            }
        }
    }
}

fn continue_keyed_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    _parent_id: Option<EvalNodeId>,
    call_source: SourceSpan,
    key: Value,
    joined: Value,
    resolver: FunctionRef,
) -> Result<EvalStep, EvalError> {
    let keyed_node_id = state.scheduler.demand_keyed_node(
        resolver,
        key,
        joined,
        state.branch_context.clone(),
        call_source.clone(),
    )?;
    state.emit_current_dependency(keyed_node_id, Some(call_source.clone()));

    if let Some(result) = state.scheduler.keyed_resolution(keyed_node_id) {
        return result.map(EvalStep::Ready);
    }

    if !state.queue_child_work {
        let prepared = prepare_keyed_call(state, keyed_node_id)?;
        let mut env = prepared.env;
        return match state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, keyed_node_id, &prepared.body, &mut env)
        }) {
            Ok(EvalStep::Ready(value)) => {
                state
                    .scheduler
                    .record_node_state(keyed_node_id, EvalNodeState::Ready(value.clone()));
                let _ = state.scheduler.finalize_keyed_completion(keyed_node_id)?;
                Ok(EvalStep::Ready(value))
            }
            Ok(EvalStep::Suspended(suspension)) => {
                let pending = suspension.pending.clone();
                state.scheduler.register_suspension(suspension);
                state
                    .scheduler
                    .record_node_state(keyed_node_id, EvalNodeState::Pending(pending.clone()));
                Ok(EvalStep::Suspended(EvalSuspension {
                    resume_node: node_id,
                    pending,
                    branch_context: state.branch_context.clone(),
                    frames: Vec::new(),
                }))
            }
            Err(error) => {
                state
                    .scheduler
                    .record_node_state(keyed_node_id, EvalNodeState::Poison(error.clone()));
                Err(error)
            }
        };
    }

    Ok(wait_on_child_dependency(
        state,
        node_id,
        keyed_node_id,
        Vec::new(),
    ))
}

fn continue_branch_assumption_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    name: Arc<str>,
) -> Result<EvalStep, EvalError> {
    let Some(value) = state.branch_context().assumption(name.as_ref()).cloned() else {
        return Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!("branch assumption {name:?} is not set"),
        ));
    };
    Ok(EvalStep::Ready(value))
}

fn continue_with_assumption_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    name: Arc<str>,
    value: Value,
    resolver: FunctionRef,
) -> Result<EvalStep, EvalError> {
    let branch_context = state.branch_context().with_assumption(name, value);
    let Some(prepared) = prepare_function_call(state, &resolver, &[], Some(node_id))? else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() resolver must be a bodyful Vixen function",
        ));
    };
    let mut env = prepared.env;
    let prior_branch_context = state.branch_context.clone();
    let result = state.with_branch_context(branch_context, |state| {
        state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, node_id, &prepared.body, &mut env)
        })
    })?;
    match result {
        EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
        EvalStep::Suspended(suspension) => Ok(extend_suspension(
            suspension,
            ContinuationFrame::RestoreBranchContext {
                branch_context: prior_branch_context,
            },
        )),
    }
}

fn continue_call_from_args(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    callee: Value,
    call_source: SourceSpan,
    type_args: Arc<[Ty]>,
    mut args: Vec<Value>,
    remaining_args: Vec<ExprHandle>,
    mut env: EvalEnv,
) -> Result<EvalStep, EvalError> {
    if state.current_parent() != Some(node_id) {
        return state.with_active_node(node_id, |state| {
            continue_call_from_args(
                state,
                node_id,
                parent_id,
                callee,
                call_source,
                type_args,
                args,
                remaining_args,
                env,
            )
        });
    }
    let mut remaining = remaining_args.into_iter();
    while let Some(arg_handle) = remaining.next() {
        match eval_dependent_expr_handle(state, node_id, arg_handle, &mut env)? {
            EvalStep::Ready(value) => args.push(value),
            EvalStep::Suspended(suspension) => {
                let remaining_args = remaining.collect::<Vec<_>>();
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::CallAfterArg {
                        node_id,
                        parent_id,
                        callee: callee.clone(),
                        call_source,
                        type_args: Arc::clone(&type_args),
                        args,
                        remaining_args,
                        env,
                    },
                ));
            }
        }
    }
    let ValueRep::Function(function) = &callee.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "call target must evaluate to a function value",
        ));
    };
    if let Some(prepared) = prepare_function_call(state, function, &args, Some(node_id))? {
        let mut env = prepared.env;
        return state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, node_id, &prepared.body, &mut env)
        });
    }
    let callee_name = function.name.to_string();
    if let Some((slot_id, witness, action, resolver)) = slot_builtin_action(&callee_name, &args)? {
        return continue_slot_builtin(
            state,
            node_id,
            parent_id,
            call_source,
            slot_id,
            witness,
            action,
            resolver,
        );
    }
    if let Some((key, joined, resolver)) = keyed_builtin_action(&callee_name, &args)? {
        return continue_keyed_builtin(
            state,
            node_id,
            parent_id,
            call_source,
            key,
            joined,
            resolver,
        );
    }
    if let Some(name) = branch_assumption_builtin_action(&callee_name, &args)? {
        return continue_branch_assumption_builtin(state, name);
    }
    if let Some((name, value, resolver)) = with_assumption_builtin_action(&callee_name, &args)? {
        return continue_with_assumption_builtin(state, node_id, name, value, resolver);
    }
    if let Some((name, candidates, resolver)) =
        search_assumption_builtin_action(&callee_name, &args)?
    {
        return continue_search_assumption_builtin(
            state,
            node_id,
            parent_id,
            name,
            candidates,
            resolver,
            0,
            Vec::new(),
            0,
            0,
        );
    }
    if let Some(step) =
        continue_builtin_call(state, node_id, parent_id, &callee_name, &type_args, &args)?
    {
        return Ok(step);
    }
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
                Some(call_source.clone()),
                EvalEventKind::RuntimeCallFinishedReady {
                    reuse: value.meta.reuse.clone(),
                    crossing: value.meta.crossing.clone(),
                },
            );
            Ok(EvalStep::Ready(value))
        }
        RuntimeCallOutcome::ReadyWithEvents(RuntimeReady { value, events }) => {
            for event in events {
                state.emit(node_id, parent_id, Some(call_source.clone()), event);
            }
            state.emit(
                node_id,
                parent_id,
                Some(call_source.clone()),
                EvalEventKind::RuntimeCallFinishedReady {
                    reuse: value.meta.reuse.clone(),
                    crossing: value.meta.crossing.clone(),
                },
            );
            Ok(EvalStep::Ready(value))
        }
        RuntimeCallOutcome::Pending { description } => {
            let pending = state.scheduler.alloc_pending(description);
            state.scheduler.record_node_state(
                EvalNodeId(pending.node_id),
                EvalNodeState::Pending(pending.clone()),
            );
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
                Some(call_source.clone()),
                EvalEventKind::RuntimeCallFinishedPending {
                    pending: pending.clone(),
                },
            );
            Ok(wrap_suspended(
                state.branch_context.clone(),
                pending,
                node_id,
                ContinuationFrame::RuntimeCall {
                    node_id,
                    parent_id,
                    call_source,
                },
            ))
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

fn eval_struct_literal(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    struct_literal: &StructLiteralExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let type_name = struct_literal
        .name()
        .map(|name| Arc::<str>::from(name.text()))
        .ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "struct literal has no type name")
        })?;
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("struct literal should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let struct_source = state.span_for_node(struct_literal);
    let remaining_fields = struct_literal
        .fields()
        .map(|field| {
            let name = Arc::<str>::from(
                field
                    .name()
                    .map(|name| name.text().to_string())
                    .ok_or_else(|| {
                        EvalError::new(EvalErrorKind::Internal, "struct field has no name")
                    })?,
            );
            let expr = field.expr().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "struct field has no value")
            })?;
            Ok((name, state.expr_handle(&expr)))
        })
        .collect::<Result<Vec<_>, EvalError>>()?;
    if state.queue_child_work && !remaining_fields.is_empty() {
        let field_names = remaining_fields
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let child_nodes = remaining_fields
            .iter()
            .map(|(_, expr)| queue_child_expr_handle(state, *expr, env))
            .collect::<Vec<_>>();
        return Ok(wait_on_child_dependency(
            state,
            node_id,
            child_nodes[0],
            vec![ContinuationFrame::StructJoin {
                node_id,
                parent_id,
                type_name,
                field_names,
                child_nodes,
                struct_source,
            }],
        ));
    }
    continue_struct_fields(
        state,
        node_id,
        parent_id,
        type_name,
        Vec::new(),
        remaining_fields,
        env.clone(),
        struct_source,
    )
}

fn eval_block(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    block: &BlockExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("block should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    continue_block_statements(
        state,
        node_id,
        parent_id,
        env.clone(),
        Value::unit(),
        block
            .statements()
            .map(|stmt| state.stmt_handle(&stmt))
            .collect(),
    )
}

fn eval_array(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    array: &ArrayExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("array should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    if state.queue_child_work {
        let mut child_nodes = Vec::new();
        for item in array.items() {
            match item {
                ArrayItem::Expr(expr) => child_nodes.push(queue_child_expr(state, &expr, env)),
                ArrayItem::Spread(_) => {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        "array spread is not supported yet",
                    ));
                }
            }
        }
        if child_nodes.is_empty() {
            let provenance = Provenance::literal(state.span_for_node(array));
            return Ok(EvalStep::Ready(Value::array(Vec::new()).with_meta(
                ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue),
            )));
        }
        return Ok(wait_on_child_dependency(
            state,
            node_id,
            child_nodes[0],
            vec![ContinuationFrame::ArrayJoin {
                node_id,
                parent_id,
                child_nodes,
                array_source: state.span_for_node(array),
            }],
        ));
    }
    continue_array_items(
        state,
        node_id,
        parent_id,
        Vec::new(),
        array
            .items()
            .map(|item| match item {
                ArrayItem::Expr(expr) => Ok(state.expr_handle(&expr)),
                ArrayItem::Spread(_) => Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "array spread is not supported yet",
                )),
            })
            .collect::<Result<Vec<_>, EvalError>>()?,
        env.clone(),
        state.span_for_node(array),
    )
}

fn eval_map(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    map: &MapExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("map should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let entries = map
        .entries()
        .map(|entry| {
            let key_expr = entry.key().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "map entry has no explicit key")
            })?;
            let value_expr = entry
                .value()
                .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "map entry has no value"))?;
            Ok((state.expr_handle(&key_expr), state.expr_handle(&value_expr)))
        })
        .collect::<Result<Vec<_>, EvalError>>()?;
    continue_map_entries(
        state,
        node_id,
        parent_id,
        Vec::new(),
        entries,
        env.clone(),
        state.span_for_node(map),
    )
}

fn eval_fail(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    fail: &FailExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("fail expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let failure = match eval_child_expr(
        state,
        &fail.message().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "fail expression has no message")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::FailMessage { node_id, parent_id },
            ));
        }
    };
    Err(user_fail_error(failure))
}

fn user_fail_error(value: Value) -> EvalError {
    match &value.rep {
        ValueRep::String(message) => EvalError::new(EvalErrorKind::UserFail, message.clone()),
        ValueRep::Struct(struct_value) => {
            EvalError::new(EvalErrorKind::UserFail, struct_value.type_name.clone())
                .with_detail(value)
        }
        ValueRep::Unit => EvalError::new(EvalErrorKind::UserFail, "Unit").with_detail(value),
        ValueRep::Bool(_) => EvalError::new(EvalErrorKind::UserFail, "Bool").with_detail(value),
        ValueRep::U64(_) => EvalError::new(EvalErrorKind::UserFail, "U64").with_detail(value),
        ValueRep::Array(_) => EvalError::new(EvalErrorKind::UserFail, "Array").with_detail(value),
        ValueRep::Map(_) => EvalError::new(EvalErrorKind::UserFail, "Map").with_detail(value),
        ValueRep::External(external) => {
            EvalError::new(EvalErrorKind::UserFail, external.kind.clone()).with_detail(value)
        }
        ValueRep::Function(function) => EvalError::new(
            EvalErrorKind::UserFail,
            format!("{}::{}", function.source_id, function.name),
        )
        .with_detail(value),
    }
}

fn eval_if(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    if_expr: &IfExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("if expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let condition = match eval_child_expr(
        state,
        &if_expr.condition().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "if expression has no condition")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::IfCondition {
                    node_id,
                    parent_id,
                    then_branch: if_expr
                        .then_branch()
                        .ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "if branch is missing")
                        })
                        .map(|expr| state.expr_handle(&expr))?,
                    else_branch: if_expr
                        .else_branch()
                        .ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "if branch is missing")
                        })
                        .map(|expr| state.expr_handle(&expr))?,
                    env: env.clone(),
                },
            ));
        }
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
    match eval_child_expr(state, &branch, env)? {
        EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
        EvalStep::Suspended(suspension) => Ok(extend_suspension(
            suspension,
            ContinuationFrame::FinishNode { node_id, parent_id },
        )),
    }
}

fn eval_field(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    field: &FieldExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    if let Some(value) = static_namespace_member_value(state, field, env)? {
        return Ok(EvalStep::Ready(value));
    }
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("field expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let receiver = match eval_child_expr(
        state,
        &field.receiver().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "field expression has no receiver")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            let field_name = field
                .field_name()
                .map(|name| name.text().to_string())
                .ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "field expression has no field name",
                    )
                })?;
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::FieldReceiver {
                    node_id,
                    parent_id,
                    field_name,
                },
            ));
        }
    };
    project_struct_field(
        receiver,
        field
            .field_name()
            .map(|name| name.text().to_string())
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "field expression has no field name",
                )
            })?,
    )
    .map(EvalStep::Ready)
}

fn eval_binary(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    binary: &BinaryExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("binary expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let lhs = binary
        .lhs()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "binary expression has no lhs"))?;
    let rhs = binary
        .rhs()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "binary expression has no rhs"))?;
    let operator = binary_operator(binary)?;
    match eval_child_expr(state, &lhs, env)? {
        EvalStep::Ready(lhs) => continue_binary_rhs(
            state,
            node_id,
            parent_id,
            lhs,
            operator,
            state.expr_handle(&rhs),
            env.clone(),
        ),
        EvalStep::Suspended(suspension) => Ok(extend_suspension(
            suspension,
            ContinuationFrame::BinaryLhs {
                node_id,
                parent_id,
                operator,
                rhs: state.expr_handle(&rhs),
                env: env.clone(),
            },
        )),
    }
}

fn continue_block_statements(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut scope: EvalEnv,
    mut last: Value,
    statements: Vec<StmtHandle>,
) -> Result<EvalStep, EvalError> {
    let mut remaining = statements.into_iter();
    while let Some(statement_handle) = remaining.next() {
        let statement = state.program.resolve_stmt(statement_handle.key)?;
        match statement {
            Stmt::Let(let_stmt) => {
                let name_token = let_stmt.name().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no name")
                })?;
                let name = name_token.text().to_string();
                let name_source = state.with_source(statement_handle.key.source(), |state| {
                    state.span_for_token(&name_token)
                });
                let expr = let_stmt.expr().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no value")
                })?;
                match state.with_source(statement_handle.key.source(), |state| {
                    eval_dependent_expr(state, node_id, &expr, &mut scope)
                })? {
                    EvalStep::Ready(value) => {
                        env_mut(&mut scope).insert(
                            name.clone(),
                            value.with_provenance(Provenance::binding(name_source.clone(), name)),
                        );
                    }
                    EvalStep::Suspended(suspension) => {
                        return Ok(extend_suspension(
                            suspension,
                            ContinuationFrame::BlockAfterLet {
                                node_id,
                                parent_id,
                                scope,
                                last,
                                name,
                                name_source,
                                remaining_statements: remaining.collect(),
                            },
                        ));
                    }
                }
            }
            Stmt::FnDecl(function) => {
                let (name, _name_source, value) = state
                    .with_source(statement_handle.key.source(), |state| {
                        bind_named_function_decl(state, &function, &scope)
                    })?;
                env_mut(&mut scope).insert(name, value);
            }
            Stmt::Expr(expr) => match state.with_source(statement_handle.key.source(), |state| {
                eval_dependent_expr(state, node_id, &expr, &mut scope)
            })? {
                EvalStep::Ready(value) => last = value,
                EvalStep::Suspended(suspension) => {
                    return Ok(extend_suspension(
                        suspension,
                        ContinuationFrame::BlockAfterExpr {
                            node_id,
                            parent_id,
                            scope,
                            remaining_statements: remaining.collect(),
                        },
                    ));
                }
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

fn continue_array_items(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut items: Vec<Value>,
    remaining_items: Vec<ExprHandle>,
    mut env: EvalEnv,
    array_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_items.into_iter();
    while let Some(item_handle) = remaining.next() {
        match eval_dependent_expr_handle(state, node_id, item_handle, &mut env)? {
            EvalStep::Ready(value) => items.push(value),
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayAfterItem {
                        node_id,
                        parent_id,
                        items,
                        remaining_items: remaining.collect(),
                        env,
                        array_source,
                    },
                ));
            }
        }
    }
    let provenance = Provenance::literal(array_source);
    let meta = aggregate_child_meta(provenance, items.iter());
    Ok(EvalStep::Ready(Value::array(items).with_meta(meta)))
}

fn queue_child_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &EvalEnv,
) -> EvalNodeId {
    queue_child_expr_handle(state, state.expr_handle(expr), env)
}

fn queue_child_expr_handle(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: ExprHandle,
    env: &EvalEnv,
) -> EvalNodeId {
    let child_id = state.alloc_node_id();
    let resolved = state
        .program
        .resolve_expr(expr.key)
        .expect("queued child expression handle should resolve");
    let resolved_source = state.program.span_for_handle(expr.key);
    state.emit_current_dependency(child_id, Some(resolved_source.clone()));
    state.scheduler.record_node(EvalNodeRecord {
        node_id: child_id,
        parent_id: state.current_parent(),
        kind: EvalNodeKind::Expr {
            kind: expr_kind(&resolved),
        },
        source: Some(resolved_source),
    });
    let scheduled = ScheduledExpr {
        expr,
        env: Arc::clone(env),
    };
    state
        .scheduler
        .remember_expr_task(child_id, scheduled.clone(), state.branch_context.clone());
    let work = state.scheduler.enqueue_ready_task(
        ReadyTask::EvaluateExpr(scheduled),
        EvalWorkKind::EvaluateExpr,
        child_id,
        None,
        state.branch_context.clone(),
    );
    state.scheduler.record_node_state(
        child_id,
        EvalNodeState::Runnable {
            work: work.id,
            pending: None,
        },
    );
    child_id
}

fn wait_on_child_dependency(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    resume_node: EvalNodeId,
    dependency: EvalNodeId,
    frames: Vec<ContinuationFrame>,
) -> EvalStep {
    let pending = state.scheduler.alloc_child_dependency_pending(dependency);
    EvalStep::Suspended(EvalSuspension {
        resume_node,
        pending,
        branch_context: state.branch_context.clone(),
        frames,
    })
}

fn continue_map_entries(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut entries: Vec<(Value, Value)>,
    remaining_entries: Vec<(ExprHandle, ExprHandle)>,
    mut env: EvalEnv,
    map_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_entries.into_iter();
    while let Some((key_handle, value_handle)) = remaining.next() {
        let key = match eval_dependent_expr_handle(state, node_id, key_handle, &mut env)? {
            EvalStep::Ready(value) => value,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::MapAfterKey {
                        node_id,
                        parent_id,
                        entries,
                        value_expr: value_handle,
                        remaining_entries: remaining.collect(),
                        env,
                        map_source,
                    },
                ));
            }
        };
        let value = match eval_dependent_expr_handle(state, node_id, value_handle, &mut env)? {
            EvalStep::Ready(value) => value,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::MapAfterValue {
                        node_id,
                        parent_id,
                        entries,
                        key,
                        remaining_entries: remaining.collect(),
                        env,
                        map_source,
                    },
                ));
            }
        };
        entries.push((key, value));
    }
    let provenance = Provenance::literal(map_source);
    let meta = aggregate_map_entries_meta(provenance, entries.iter());
    Ok(EvalStep::Ready(Value::map(entries).with_meta(meta)))
}

fn continue_struct_fields(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    type_name: Arc<str>,
    mut fields: Vec<(Arc<str>, Value)>,
    remaining_fields: Vec<(Arc<str>, ExprHandle)>,
    mut env: EvalEnv,
    struct_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_fields.into_iter();
    while let Some((name, expr_handle)) = remaining.next() {
        match eval_dependent_expr_handle(state, node_id, expr_handle, &mut env)? {
            EvalStep::Ready(value) => fields.push((name, value)),
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::StructAfterField {
                        node_id,
                        parent_id,
                        type_name,
                        fields,
                        name,
                        remaining_fields: remaining.collect(),
                        env,
                        struct_source,
                    },
                ));
            }
        }
    }
    let provenance = Provenance::literal(struct_source);
    let meta = aggregate_child_meta(provenance, fields.iter().map(|(_, value)| value));
    Ok(EvalStep::Ready(
        Value::structure(type_name, fields).with_meta(meta),
    ))
}

fn project_struct_field(receiver: Value, field_name: String) -> Result<Value, EvalError> {
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

fn resume_suspension(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    suspension: EvalSuspension,
    result: Result<Value, EvalError>,
) -> Result<EvalStep, EvalError> {
    let mut current = result;
    let resume_node = suspension.resume_node;
    let parent_id = state
        .scheduler
        .node_record(resume_node)
        .and_then(|record| record.parent_id);
    let frames = suspension.frames;
    let mut index = 0;
    while index < frames.len() {
        match resume_frame(state, frames[index].clone(), current)? {
            ResumeOutcome::Continue(next) => current = next,
            ResumeOutcome::Suspended(mut new_suspension) => {
                new_suspension
                    .frames
                    .extend(frames[index + 1..].iter().cloned());
                return Ok(EvalStep::Suspended(new_suspension));
            }
        }
        index += 1;
    }
    if !matches!(
        state.scheduler.node_state(resume_node),
        Some(EvalNodeState::Ready(_)) | Some(EvalNodeState::Poison(_))
    ) {
        match &current {
            Ok(value) => record_ready_step(state, resume_node, parent_id, value),
            Err(error) => record_poison_step(state, resume_node, parent_id, error),
        }
    }
    match current {
        Ok(value) => Ok(EvalStep::Ready(value)),
        Err(error) => Err(error),
    }
}

fn resume_frame(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    frame: ContinuationFrame,
    input: Result<Value, EvalError>,
) -> Result<ResumeOutcome, EvalError> {
    match frame {
        ContinuationFrame::FinishNode { node_id, parent_id } => Ok(match input {
            Ok(value) => {
                record_ready_step(state, node_id, parent_id, &value);
                ResumeOutcome::Continue(Ok(value))
            }
            Err(error) => {
                record_poison_step(state, node_id, parent_id, &error);
                ResumeOutcome::Continue(Err(error))
            }
        }),
        ContinuationFrame::RuntimeCall {
            node_id,
            parent_id,
            call_source,
        } => Ok(match input {
            Ok(value) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(call_source),
                    EvalEventKind::RuntimeCallFinishedReady {
                        reuse: value.meta.reuse.clone(),
                        crossing: value.meta.crossing.clone(),
                    },
                );
                record_ready_step(state, node_id, parent_id, &value);
                ResumeOutcome::Continue(Ok(value))
            }
            Err(error) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(call_source),
                    EvalEventKind::RuntimeCallFinishedPoison {
                        kind: error.kind,
                        message: error.message.clone(),
                    },
                );
                record_poison_step(state, node_id, parent_id, &error);
                ResumeOutcome::Continue(Err(error))
            }
        }),
        ContinuationFrame::SlotAfterResolver {
            node_id,
            parent_id,
            call_source,
            slot_id,
            witness,
            action,
        } => {
            let result = match input {
                Ok(value) => commit_slot_result(
                    state,
                    node_id,
                    parent_id,
                    &call_source,
                    &slot_id,
                    &witness,
                    &action,
                    &value,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::CallJoin {
            node_id,
            parent_id,
            callee,
            call_source,
            type_args,
            child_nodes,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut args = Vec::with_capacity(child_nodes.len());
                    for child_node in &child_nodes {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => args.push(value.clone()),
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(*child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    branch_context: state.branch_context.clone(),
                                    frames: vec![ContinuationFrame::CallJoin {
                                        node_id,
                                        parent_id,
                                        callee: callee.clone(),
                                        call_source,
                                        type_args: Arc::clone(&type_args),
                                        child_nodes: child_nodes.clone(),
                                    }],
                                }));
                            }
                        }
                    }
                    continue_call_from_args(
                        state,
                        node_id,
                        parent_id,
                        callee,
                        call_source,
                        type_args,
                        args,
                        Vec::new(),
                        empty_env(),
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::CallAfterCallee {
            node_id,
            parent_id,
            call_source,
            type_args,
            env,
            args,
            remaining_args,
        } => {
            let result = match input {
                Ok(callee) => continue_call_from_args(
                    state,
                    node_id,
                    parent_id,
                    callee,
                    call_source,
                    type_args,
                    args,
                    remaining_args,
                    env,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BlockAfterLet {
            node_id,
            parent_id,
            mut scope,
            last,
            name,
            name_source,
            remaining_statements,
        } => {
            let result = match input {
                Ok(value) => {
                    env_mut(&mut scope).insert(
                        name.clone(),
                        value.with_provenance(Provenance::binding(name_source, name)),
                    );
                    continue_block_statements(
                        state,
                        node_id,
                        parent_id,
                        scope,
                        last,
                        remaining_statements,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BlockAfterExpr {
            node_id,
            parent_id,
            scope,
            remaining_statements,
        } => {
            let result = match input {
                Ok(value) => continue_block_statements(
                    state,
                    node_id,
                    parent_id,
                    scope,
                    value,
                    remaining_statements,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::CallAfterArg {
            node_id,
            parent_id,
            callee,
            call_source,
            type_args,
            mut args,
            remaining_args,
            env,
        } => {
            let result = match input {
                Ok(value) => {
                    args.push(value);
                    continue_call_from_args(
                        state,
                        node_id,
                        parent_id,
                        callee,
                        call_source,
                        type_args,
                        args,
                        remaining_args,
                        env,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayAfterItem {
            node_id,
            parent_id,
            mut items,
            remaining_items,
            env,
            array_source,
        } => {
            let result = match input {
                Ok(value) => {
                    items.push(value);
                    continue_array_items(
                        state,
                        node_id,
                        parent_id,
                        items,
                        remaining_items,
                        env,
                        array_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayJoin {
            node_id,
            parent_id,
            child_nodes,
            array_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut items = Vec::with_capacity(child_nodes.len());
                    for child_node in &child_nodes {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => items.push(value.clone()),
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(*child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    branch_context: state.branch_context.clone(),
                                    frames: vec![ContinuationFrame::ArrayJoin {
                                        node_id,
                                        parent_id,
                                        child_nodes: child_nodes.clone(),
                                        array_source: array_source.clone(),
                                    }],
                                }));
                            }
                        }
                    }
                    let provenance = Provenance::literal(array_source);
                    let meta = aggregate_child_meta(provenance, items.iter());
                    Ok(EvalStep::Ready(Value::array(items).with_meta(meta)))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::MapAfterKey {
            node_id,
            parent_id,
            entries,
            value_expr,
            remaining_entries,
            mut env,
            map_source,
        } => {
            let result = match input {
                Ok(key) => {
                    let value =
                        match eval_dependent_expr_handle(state, node_id, value_expr, &mut env)? {
                            EvalStep::Ready(value) => value,
                            EvalStep::Suspended(suspension) => {
                                return Ok(ResumeOutcome::Suspended(extend_resume(
                                    suspension,
                                    ContinuationFrame::MapAfterValue {
                                        node_id,
                                        parent_id,
                                        entries,
                                        key,
                                        remaining_entries,
                                        env,
                                        map_source,
                                    },
                                )));
                            }
                        };
                    let mut entries = entries;
                    entries.push((key, value));
                    continue_map_entries(
                        state,
                        node_id,
                        parent_id,
                        entries,
                        remaining_entries,
                        env,
                        map_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::MapAfterValue {
            node_id,
            parent_id,
            mut entries,
            key,
            remaining_entries,
            env,
            map_source,
        } => {
            let result = match input {
                Ok(value) => {
                    entries.push((key, value));
                    continue_map_entries(
                        state,
                        node_id,
                        parent_id,
                        entries,
                        remaining_entries,
                        env,
                        map_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::FailMessage { node_id, parent_id } => {
            let result = match input {
                Ok(value) => Err(user_fail_error(value)),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::RestoreBranchContext { branch_context } => {
            state.branch_context = branch_context;
            Ok(ResumeOutcome::Continue(input))
        }
        ContinuationFrame::SearchAssumptionAfterBranch {
            node_id,
            parent_id,
            name,
            candidates,
            resolver,
            index,
            mut attempts,
            root_search_start,
            search_start_index,
            prior_branch_context,
        } => {
            state.branch_context = prior_branch_context.clone();
            match input {
                Ok(value) => Ok(ResumeOutcome::Continue(Ok(value))),
                Err(error) if error.kind == EvalErrorKind::UserFail => {
                    learn_point_no_good(
                        state,
                        &prior_branch_context,
                        &name,
                        &candidates,
                        index,
                        &resolver,
                        &error,
                    )?;
                    attempts.push(branch_attempt_value(candidates[index].clone(), error));
                    let result = continue_search_assumption_builtin(
                        state,
                        node_id,
                        parent_id,
                        name.clone(),
                        candidates.clone(),
                        resolver.clone(),
                        index + 1,
                        attempts,
                        root_search_start,
                        index + 1,
                    );
                    if let Err(error) = &result
                        && search_start_index < candidates.len()
                    {
                        let local_attempt_offset =
                            search_start_index.saturating_sub(root_search_start);
                        let local_error =
                            local_branch_conflict_error(name.clone(), error, local_attempt_offset)
                                .unwrap_or_else(|| error.clone());
                        learn_search_no_good(
                            state,
                            &prior_branch_context,
                            &name,
                            &candidates[search_start_index..],
                            &resolver,
                            &local_error,
                        )?;
                    }
                    Ok(conclude_resumed_node(state, node_id, parent_id, result))
                }
                Err(error) => Ok(ResumeOutcome::Continue(Err(error))),
            }
        }
        ContinuationFrame::IfCondition {
            node_id,
            parent_id,
            then_branch,
            else_branch,
            mut env,
        } => {
            let result = match input {
                Ok(condition) => {
                    let ValueRep::Bool(value) = condition.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "if condition must evaluate to Bool",
                        ));
                    };
                    match eval_dependent_expr_handle(
                        state,
                        node_id,
                        if value { then_branch } else { else_branch },
                        &mut env,
                    )? {
                        EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
                        EvalStep::Suspended(suspension) => Ok(extend_suspension(
                            suspension,
                            ContinuationFrame::FinishNode { node_id, parent_id },
                        )),
                    }
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BinaryLhs {
            node_id,
            parent_id,
            operator,
            rhs,
            env,
        } => {
            let result = match input {
                Ok(lhs) => continue_binary_rhs(state, node_id, parent_id, lhs, operator, rhs, env),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BinaryRhs {
            node_id,
            parent_id,
            lhs,
            operator,
        } => {
            let result = match input {
                Ok(rhs) => apply_binary_operator(lhs, operator, rhs).map(EvalStep::Ready),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StructAfterField {
            node_id,
            parent_id,
            type_name,
            mut fields,
            name,
            remaining_fields,
            env,
            struct_source,
        } => {
            let result = match input {
                Ok(value) => {
                    fields.push((name, value));
                    continue_struct_fields(
                        state,
                        node_id,
                        parent_id,
                        type_name,
                        fields,
                        remaining_fields,
                        env,
                        struct_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StructJoin {
            node_id,
            parent_id,
            type_name,
            field_names,
            child_nodes,
            struct_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut fields = Vec::with_capacity(child_nodes.len());
                    for (name, child_node) in field_names.iter().zip(child_nodes.iter()) {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => {
                                fields.push((name.clone(), value.clone()))
                            }
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(*child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    branch_context: state.branch_context.clone(),
                                    frames: vec![ContinuationFrame::StructJoin {
                                        node_id,
                                        parent_id,
                                        type_name,
                                        field_names: field_names.clone(),
                                        child_nodes: child_nodes.clone(),
                                        struct_source: struct_source.clone(),
                                    }],
                                }));
                            }
                        }
                    }
                    let provenance = Provenance::literal(struct_source);
                    let meta =
                        aggregate_child_meta(provenance, fields.iter().map(|(_, value)| value));
                    Ok(EvalStep::Ready(
                        Value::structure(type_name, fields).with_meta(meta),
                    ))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayFindAfterPredicate {
            node_id,
            parent_id,
            items,
            predicate,
            index,
        } => {
            let result = match input {
                Ok(result) => match result.rep {
                    ValueRep::Bool(true) => Ok(EvalStep::Ready(
                        items[index]
                            .clone()
                            .project(ProjectionStep::Index(index as u64)),
                    )),
                    ValueRep::Bool(false) => continue_array_find_builtin(
                        state,
                        node_id,
                        parent_id,
                        items,
                        predicate,
                        index + 1,
                    ),
                    _ => Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "array_find() predicate must return Bool",
                    )),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayFoldAfterStep {
            node_id,
            parent_id,
            items,
            step,
            index,
        } => {
            let result = match input {
                Ok(accumulator) => continue_array_fold_builtin(
                    state,
                    node_id,
                    parent_id,
                    items,
                    accumulator,
                    step,
                    index + 1,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::FieldReceiver {
            node_id,
            parent_id,
            field_name,
        } => {
            let result = input.and_then(|receiver| project_struct_field(receiver, field_name));
            Ok(match result {
                Ok(value) => {
                    record_ready_step(state, node_id, parent_id, &value);
                    ResumeOutcome::Continue(Ok(value))
                }
                Err(error) => {
                    record_poison_step(state, node_id, parent_id, &error);
                    ResumeOutcome::Continue(Err(error))
                }
            })
        }
    }
}

fn extend_resume(mut suspension: EvalSuspension, frame: ContinuationFrame) -> EvalSuspension {
    suspension.frames.push(frame);
    suspension
}

fn expr_kind(expr: &Expr) -> EvalExprKind {
    match expr {
        Expr::Block(_) => EvalExprKind::Block,
        Expr::Literal(_) => EvalExprKind::Literal,
        Expr::Paren(_) => EvalExprKind::Paren,
        Expr::Closure(_) => EvalExprKind::Closure,
        Expr::NameRef(_) => EvalExprKind::NameRef,
        Expr::Call(_) => EvalExprKind::Call,
        Expr::Array(_) => EvalExprKind::Array,
        Expr::Map(_) => EvalExprKind::Map,
        Expr::Fail(_) => EvalExprKind::Fail,
        Expr::If(_) => EvalExprKind::If,
        Expr::Binary(_) => EvalExprKind::Binary,
        Expr::StructLiteral(_) => EvalExprKind::StructLiteral,
        Expr::Field(_) => EvalExprKind::Field,
        _ => EvalExprKind::Unsupported,
    }
}

fn binary_operator(binary: &BinaryExpr) -> Result<SyntaxKind, EvalError> {
    binary
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .map(|token| token.kind())
        .find(|kind| matches!(kind, SyntaxKind::EQ2 | SyntaxKind::BANG_EQ))
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "binary expression has no operator"))
}

fn continue_binary_rhs(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    lhs: Value,
    operator: SyntaxKind,
    rhs: ExprHandle,
    mut env: EvalEnv,
) -> Result<EvalStep, EvalError> {
    match eval_dependent_expr_handle(state, node_id, rhs, &mut env)? {
        EvalStep::Ready(rhs) => apply_binary_operator(lhs, operator, rhs).map(EvalStep::Ready),
        EvalStep::Suspended(suspension) => Ok(extend_suspension(
            suspension,
            ContinuationFrame::BinaryRhs {
                node_id,
                parent_id,
                lhs,
                operator,
            },
        )),
    }
}

fn apply_binary_operator(lhs: Value, operator: SyntaxKind, rhs: Value) -> Result<Value, EvalError> {
    let equals = values_equal(&lhs.rep, &rhs.rep)?;
    match operator {
        SyntaxKind::EQ2 => Ok(Value::bool(equals)),
        SyntaxKind::BANG_EQ => Ok(Value::bool(!equals)),
        _ => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("binary operator {operator:?} is not supported yet"),
        )),
    }
}

fn values_equal(lhs: &ValueRep, rhs: &ValueRep) -> Result<bool, EvalError> {
    match (lhs, rhs) {
        (ValueRep::Unit, ValueRep::Unit) => Ok(true),
        (ValueRep::Bool(lhs), ValueRep::Bool(rhs)) => Ok(lhs == rhs),
        (ValueRep::U64(lhs), ValueRep::U64(rhs)) => Ok(lhs == rhs),
        (ValueRep::String(lhs), ValueRep::String(rhs)) => Ok(lhs == rhs),
        (ValueRep::Array(lhs), ValueRep::Array(rhs)) => {
            if lhs.len() != rhs.len() {
                return Ok(false);
            }
            lhs.iter()
                .zip(rhs.iter())
                .try_fold(true, |equal, (lhs, rhs)| {
                    Ok(equal && values_equal(&lhs.rep, &rhs.rep)?)
                })
        }
        (ValueRep::Map(lhs), ValueRep::Map(rhs)) => {
            if lhs.entries.len() != rhs.entries.len() {
                return Ok(false);
            }
            lhs.entries.iter().zip(rhs.entries.iter()).try_fold(
                true,
                |equal, ((lhs_key, lhs_value), (rhs_key, rhs_value))| {
                    Ok(equal
                        && values_equal(&lhs_key.rep, &rhs_key.rep)?
                        && values_equal(&lhs_value.rep, &rhs_value.rep)?)
                },
            )
        }
        (ValueRep::Struct(lhs), ValueRep::Struct(rhs)) => {
            if lhs.type_name != rhs.type_name || lhs.fields.len() != rhs.fields.len() {
                return Ok(false);
            }
            lhs.fields.iter().zip(rhs.fields.iter()).try_fold(
                true,
                |equal, ((lhs_name, lhs_value), (rhs_name, rhs_value))| {
                    Ok(equal
                        && lhs_name == rhs_name
                        && values_equal(&lhs_value.rep, &rhs_value.rep)?)
                },
            )
        }
        (ValueRep::External(_), _) | (_, ValueRep::External(_)) => Err(EvalError::new(
            EvalErrorKind::TypeError,
            "external values do not support authored equality yet",
        )),
        (ValueRep::Function(_), _) | (_, ValueRep::Function(_)) => Err(EvalError::new(
            EvalErrorKind::TypeError,
            "function values do not support authored equality",
        )),
        _ => Ok(false),
    }
}

fn join_keyed_input(existing: &Value, incoming: &Value) -> Result<(Value, bool), EvalError> {
    match (&existing.rep, &incoming.rep) {
        (ValueRep::Array(existing_items), ValueRep::Array(incoming_items)) => {
            let mut items = existing_items.iter().cloned().collect::<Vec<_>>();
            let mut widened = false;
            for item in incoming_items.iter() {
                let mut already_present = false;
                for existing in &items {
                    if values_equal(&existing.rep, &item.rep)? {
                        already_present = true;
                        break;
                    }
                }
                if !already_present {
                    items.push(item.clone());
                    widened = true;
                }
            }
            let meta = aggregate_array_items_meta(existing.meta.provenance.clone(), items.iter());
            Ok((Value::array(items).with_meta(meta), widened))
        }
        _ => {
            let equal = values_equal(&existing.rep, &incoming.rep)?;
            if equal {
                Ok((existing.clone(), false))
            } else {
                Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "keyed() joined input must widen monotonically",
                ))
            }
        }
    }
}

fn merge_keyed_node(
    existing: &mut KeyedNodeMemo,
    incoming: KeyedNodeMemo,
) -> Result<(), EvalError> {
    if existing.resolver != incoming.resolver {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!(
                "keyed node {:?} diverged during completion merge",
                incoming.node_id
            ),
        ));
    }
    if existing.branch_context != incoming.branch_context {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!(
                "keyed node {:?} changed branch context during completion merge",
                incoming.node_id
            ),
        ));
    }
    if !values_equal(&existing.key.rep, &incoming.key.rep)? {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!(
                "keyed node {:?} changed key during completion merge",
                incoming.node_id
            ),
        ));
    }
    let incoming_revision = incoming.revision;
    let (joined, _) = join_keyed_input(&existing.joined, &incoming.joined)?;
    existing.joined = joined;
    if incoming_revision >= existing.revision {
        existing.last_update = incoming.last_update;
    }
    existing.revision = existing.revision.max(incoming_revision);
    existing.active_revision = match (existing.active_revision, incoming.active_revision) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left @ Some(_), None) => left,
        (None, right @ Some(_)) => right,
        (None, None) => None,
    };
    existing.evaluated_revision = match (existing.evaluated_revision, incoming.evaluated_revision) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left @ Some(_), None) => left,
        (None, right @ Some(_)) => right,
        (None, None) => None,
    };
    Ok(())
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

fn eval_literal(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    literal: &LiteralExpr,
) -> Result<Value, EvalError> {
    let token = literal
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "literal has no token"))?;
    let literal_source = state.span_for_node(literal);
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
