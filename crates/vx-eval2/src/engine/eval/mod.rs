use super::*;
use vx_parser::MatchExpr;

mod builtins;
pub(in crate::engine) use builtins::*;

mod resume;
pub(in crate::engine) use resume::*;

pub(super) struct PreparedEntry {
    pub(super) source: SourceId,
    pub(super) body: Expr,
    pub(super) env: EvalEnv,
}

pub(super) fn function_ref_from_compiled(
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

pub(super) fn captures_from_scope(scope: &EvalEnv) -> Arc<[(Arc<str>, Value)]> {
    Arc::from(
        scope
            .iter()
            .map(|(name, value)| (Arc::<str>::from(name.as_str()), value.clone()))
            .collect::<Vec<_>>(),
    )
}

pub(super) fn compile_closure_parameters(
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

fn dollar_placeholder_index(text: &str) -> Option<usize> {
    if text == "$" {
        return Some(0);
    }
    text.strip_prefix('$')?.parse().ok()
}

fn compile_adapter_parameters(
    source: SourceId,
    expr: &Expr,
    fallback: ProgramNodeHandle,
) -> Option<Arc<[CompiledParameter]>> {
    let mut names_by_index: Vec<Option<ProgramNodeHandle>> = Vec::new();
    for name_ref in expr.syntax().descendants().filter_map(NameRefExpr::cast) {
        let token = name_ref.token()?;
        if token.kind() != SyntaxKind::DOLLAR_IDENT {
            continue;
        }
        let index = dollar_placeholder_index(token.text())?;
        if names_by_index.len() <= index {
            names_by_index.resize(index + 1, None);
        }
        names_by_index[index] = Some(ProgramNodeHandle::for_token(source, &token));
    }
    if names_by_index.is_empty() {
        return None;
    }
    Some(Arc::from(
        names_by_index
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let label = if index == 0 {
                    Arc::<str>::from("$")
                } else {
                    Arc::<str>::from(format!("${index}"))
                };
                CompiledParameter {
                    label,
                    name: name.unwrap_or(fallback),
                }
            })
            .collect::<Vec<_>>(),
    ))
}

fn builtin_adapter_arg_index(function_name: &str, arg_index: usize) -> bool {
    match function_name {
        "Array.any" | "Array.filter" | "Array.map" | "Array.filter_map" | "Map.map" => {
            arg_index == 0
        }
        "array_find" => arg_index == 1,
        "array_fold" => arg_index == 2,
        _ => false,
    }
}

fn function_adapter_value_for_arg(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    callee: &Value,
    arg_index: usize,
    arg_handle: ExprHandle,
    env: &EvalEnv,
) -> Result<Option<Value>, EvalError> {
    let ValueRep::Function(function) = &callee.rep else {
        return Ok(None);
    };
    if !builtin_adapter_arg_index(function.name.as_ref(), arg_index) {
        return Ok(None);
    }
    let expr = state.program.resolve_expr(arg_handle.key)?;
    let Some(parameters) =
        compile_adapter_parameters(arg_handle.key.source(), &expr, arg_handle.key)
    else {
        return Ok(None);
    };
    let function = FunctionRef::new(
        state.program.source_id(arg_handle.key.source()),
        Arc::from("<adapter>"),
        parameters,
        false,
        Some(arg_handle.key),
        captures_from_scope(env),
        None,
    );
    Ok(Some(Value::function(function).with_provenance(
        Provenance::literal(state.program.span_for_node(arg_handle.key.source(), &expr)),
    )))
}

pub(super) fn bind_named_function_decl(
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

pub(super) fn eval_closure(
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

pub(super) fn prepare_entry(
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

pub(super) fn prepare_function_call(
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

pub(super) fn keyed_context_value(key: Value, joined: Value) -> Value {
    let fields = vec![(Arc::from("key"), key), (Arc::from("joined"), joined)];
    let meta = aggregate_child_meta(
        Provenance::unknown(SourceSpan::synthetic("<keyed-context>")),
        fields.iter().map(|(_, value)| value),
    );
    Value::structure("KeyedContext", fields).with_meta(meta)
}

pub(super) fn prepare_keyed_call(
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

pub(super) fn evaluate_entry_once(
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

pub(super) fn bind_arguments(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    parameters: &[CompiledParameter],
    args: &[Value],
    parent_id: Option<EvalNodeId>,
) -> Result<EvalEnv, EvalError> {
    let mut env = empty_env();
    bind_arguments_into_env(state, &mut env, parameters, args, parent_id)?;
    Ok(env)
}

pub(super) fn bind_arguments_into_env(
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

pub(super) fn eval_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state.alloc_node_id();
    eval_expr_with_node_id(state, node_id, expr, env)
}

pub(super) fn eval_expr_with_node_id(
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
        Expr::Tuple(tuple) => eval_tuple(state, tuple, env),
        Expr::Range(range) => eval_range(state, range, env),
        Expr::Map(map) => eval_map(state, map, env),
        Expr::Fail(fail) => eval_fail(state, fail, env),
        Expr::If(if_expr) => eval_if(state, if_expr, env),
        Expr::Match(match_expr) => eval_match(state, match_expr, env),
        Expr::Where(where_expr) => eval_where(state, where_expr, env),
        Expr::Prefix(prefix) => eval_prefix(state, prefix, env),
        Expr::Binary(binary) => eval_binary(state, binary, env),
        Expr::InterpolatedString(interpolated) => {
            eval_interpolated_string(state, interpolated, env)
        }
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

pub(super) fn eval_child_expr(
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

pub(super) fn eval_call(
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

pub(super) fn immediate_call_callee_value(
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
    let name = name_ref_text(name_ref);
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
    if let Some(constructor) = state
        .program
        .resolve_variant_constructor(name.rsplit('.').next().unwrap_or(&name))
    {
        return Ok(Some(enum_constructor_or_value(
            state,
            constructor.name,
            constructor.fields,
            state.span_for_token(&name_token),
        )));
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

pub(super) fn eval_name_ref(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    name_ref: &NameRefExpr,
    env: &EvalEnv,
) -> Result<Value, EvalError> {
    let name_token = name_ref
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "name reference has no token"))?;
    let name = name_ref_text(name_ref);
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
    if let Some(constructor) = state
        .program
        .resolve_variant_constructor(name.rsplit('.').next().unwrap_or(&name))
    {
        return Ok(enum_constructor_or_value(
            state,
            constructor.name,
            constructor.fields,
            state.span_for_token(&name_token),
        ));
    }
    Err(EvalError::new(
        EvalErrorKind::Internal,
        format!("name {name} is not bound in this scope"),
    ))
}

fn name_ref_text(name_ref: &NameRefExpr) -> String {
    name_ref
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .filter(|token| {
            matches!(
                token.kind(),
                SyntaxKind::LOWER_IDENT
                    | SyntaxKind::UPPER_IDENT
                    | SyntaxKind::DOLLAR_IDENT
                    | SyntaxKind::UNDERSCORE
                    | SyntaxKind::DOT
            )
        })
        .map(|token| token.text().to_string())
        .collect()
}

pub(super) fn eval_call_callee(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    callee: &Expr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    if let Some(value) = immediate_call_callee_value(state, callee, env)? {
        return Ok(EvalStep::Ready(value));
    }
    eval_child_expr(state, callee, env)
}

pub(super) fn static_namespace_member_value(
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
    if let Some(constructor) = state.program.resolve_variant_constructor(field_name.text()) {
        return Ok(Some(enum_constructor_or_value(
            state,
            constructor.name,
            constructor.fields,
            state.span_for_node(field),
        )));
    }
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

fn bound_method_name(receiver: &Value, field_name: &str) -> Option<&'static str> {
    match (&receiver.rep, field_name) {
        (ValueRep::External(external), "utf8") if external.kind.as_ref() == "Blob" => {
            Some("Blob.utf8")
        }
        (ValueRep::String(_), "len") => Some("String.len"),
        (ValueRep::String(_), "is_empty") => Some("String.is_empty"),
        (ValueRep::String(_), "split_once") => Some("String.split_once"),
        (ValueRep::String(_), "slice") => Some("String.slice"),
        (ValueRep::String(_), "contains") => Some("String.contains"),
        (ValueRep::String(_), "lines") => Some("String.lines"),
        (ValueRep::String(_), "replace") => Some("String.replace"),
        (ValueRep::String(_), "to_lower") => Some("String.to_lower"),
        (ValueRep::String(_), "to_u64") => Some("String.to_u64"),
        (ValueRep::String(_), "starts_with") => Some("String.starts_with"),
        (ValueRep::String(_), "ends_with") => Some("String.ends_with"),
        (ValueRep::Array(_), "any") => Some("Array.any"),
        (ValueRep::Array(_), "filter_map") => Some("Array.filter_map"),
        (ValueRep::Array(_), "flatten") => Some("Array.flatten"),
        (ValueRep::Array(_), "filter") => Some("Array.filter"),
        (ValueRep::Array(_), "map") => Some("Array.map"),
        (ValueRep::Array(_), "to_array") => Some("Array.to_array"),
        (ValueRep::Map(_), "map") => Some("Map.map"),
        (ValueRep::Map(_), "to_array") => Some("Map.to_array"),
        (ValueRep::Map(_), "get") => Some("Map.get"),
        (ValueRep::Map(_), "has") => Some("Map.has"),
        (ValueRep::Struct(StructValue { type_name, .. }), "unwrap_or")
            if matches!(type_name.as_ref(), "Some" | "None") =>
        {
            Some("Option.unwrap_or")
        }
        _ => None,
    }
}

fn bound_method_value(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    receiver: Value,
    field_name: &str,
    field_source: SourceSpan,
) -> Option<Value> {
    let method_name = bound_method_name(&receiver, field_name)?;
    let function = FunctionRef::new(
        state.program.source_id(state.current_source()),
        Arc::from(method_name),
        Arc::from([]),
        false,
        None,
        Arc::from(vec![(Arc::from("self"), receiver)]),
        None,
    );
    Some(
        Value::function(function)
            .with_provenance(Provenance::binding(field_source, method_name.to_string())),
    )
}

pub(super) fn project_field_or_method_value(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    receiver: Value,
    field_name: String,
    field_source: SourceSpan,
) -> Result<Value, EvalError> {
    if let Some(value) = bound_method_value(state, receiver.clone(), &field_name, field_source) {
        return Ok(value);
    }
    project_struct_field(receiver, field_name)
}

fn bound_method_dispatch_args(function: &FunctionRef, args: &[Value]) -> Vec<Value> {
    let Some((_, receiver)) = function
        .captures
        .iter()
        .find(|(name, _)| name.as_ref() == "self")
    else {
        return args.to_vec();
    };

    let mut dispatch_args = Vec::with_capacity(args.len() + 1);
    dispatch_args.push(receiver.clone());
    dispatch_args.extend(args.iter().cloned());
    dispatch_args
}

fn enum_constructor_or_value(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    name: Arc<str>,
    field_names: Arc<[Arc<str>]>,
    source: SourceSpan,
) -> Value {
    if field_names.is_empty() {
        return Value::structure(name.clone(), Vec::new())
            .with_provenance(Provenance::binding(source, name.to_string()));
    }
    let captures = Arc::from([
        (Arc::from("__enum_variant_ctor"), Value::bool(true)),
        (
            Arc::from("__enum_variant_fields"),
            Value::array(
                field_names
                    .iter()
                    .map(|field_name| Value::string(field_name.clone()))
                    .collect::<Vec<_>>(),
            ),
        ),
    ]);
    Value::function(FunctionRef::new(
        state.program.source_id(state.current_source()),
        name.clone(),
        Arc::from([]),
        false,
        None,
        captures,
        None,
    ))
    .with_provenance(Provenance::binding(source, name.to_string()))
}

fn enum_constructor_field_names(function: &FunctionRef) -> Option<Vec<Arc<str>>> {
    let marker = function
        .captures
        .iter()
        .find(|(name, _)| name.as_ref() == "__enum_variant_ctor")?;
    if marker.1.rep != ValueRep::Bool(true) {
        return None;
    }
    let fields = function
        .captures
        .iter()
        .find(|(name, _)| name.as_ref() == "__enum_variant_fields")?;
    let ValueRep::Array(items) = &fields.1.rep else {
        return None;
    };
    items
        .iter()
        .map(|value| match &value.rep {
            ValueRep::String(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn continue_call_from_args(
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
        let arg_index = args.len();
        let adapter = function_adapter_value_for_arg(state, &callee, arg_index, arg_handle, &env)?;
        let arg_step = match adapter {
            Some(value) => EvalStep::Ready(value),
            None => eval_dependent_expr_handle(state, node_id, arg_handle, &mut env)?,
        };
        match arg_step {
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
    if let Some(field_names) = enum_constructor_field_names(function) {
        if field_names.len() != args.len() {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                format!(
                    "enum constructor {} expected {} argument(s), got {}",
                    function.name,
                    field_names.len(),
                    args.len()
                ),
            ));
        }
        let fields = field_names.into_iter().zip(args).collect::<Vec<_>>();
        let value = Value::structure(function.name.clone(), fields)
            .with_provenance(Provenance::binding(call_source, function.name.to_string()));
        return Ok(EvalStep::Ready(value));
    }
    let args = bound_method_dispatch_args(function, &args);
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

pub(super) fn eval_struct_literal(
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
    let mut immediate_fields = Vec::new();
    let mut remaining_fields = Vec::new();
    for field in struct_literal.fields() {
        let name_token = field
            .name()
            .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "struct field has no name"))?;
        let name = Arc::<str>::from(name_token.text().to_string());
        match field.expr() {
            Some(expr) => remaining_fields.push((name, state.expr_handle(&expr))),
            None => {
                let field_name = name_token.text();
                let Some(value) = env.get(field_name) else {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        format!("struct field {field_name} has no value"),
                    ));
                };
                immediate_fields.push((name, value.clone()));
            }
        }
    }
    if state.queue_child_work && immediate_fields.is_empty() && !remaining_fields.is_empty() {
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
        immediate_fields,
        remaining_fields,
        env.clone(),
        struct_source,
    )
}

pub(super) fn eval_block(
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

pub(super) fn eval_array(
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
                ArrayItem::Expr(expr) => {
                    child_nodes.push(ArrayJoinItem::Expr(queue_child_expr(state, &expr, env)));
                }
                ArrayItem::Spread(spread) => {
                    child_nodes.push(ArrayJoinItem::Spread(queue_child_expr(
                        state,
                        &spread.expr().ok_or_else(|| {
                            EvalError::new(
                                EvalErrorKind::Internal,
                                "array spread expression has no operand",
                            )
                        })?,
                        env,
                    )))
                }
            }
        }
        if child_nodes.is_empty() {
            let provenance = Provenance::literal(state.span_for_node(array));
            return Ok(EvalStep::Ready(Value::array(Vec::new()).with_meta(
                ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue),
            )));
        }
        let first_child = match child_nodes[0] {
            ArrayJoinItem::Expr(child_id) | ArrayJoinItem::Spread(child_id) => child_id,
        };
        return Ok(wait_on_child_dependency(
            state,
            node_id,
            first_child,
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
                ArrayItem::Expr(expr) => Ok(ArrayEvalItem::Expr(state.expr_handle(&expr))),
                ArrayItem::Spread(spread) => Ok(ArrayEvalItem::Spread(state.expr_handle(
                    &spread.expr().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "array spread expression has no operand",
                        )
                    })?,
                ))),
            })
            .collect::<Result<Vec<_>, EvalError>>()?,
        env.clone(),
        state.span_for_node(array),
    )
}

fn append_array_item(items: &mut Vec<Value>, value: Value, spread: bool) -> Result<(), EvalError> {
    if spread {
        let ValueRep::Array(spread_items) = &value.rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "array spread value must evaluate to Array",
            ));
        };
        items.extend(spread_items.iter().cloned());
    } else {
        items.push(value);
    }
    Ok(())
}

pub(super) fn eval_tuple(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    tuple: &TupleExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("tuple should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let items = tuple.items().collect::<Vec<_>>();
    if state.queue_child_work {
        let child_nodes = items
            .iter()
            .map(|item| queue_child_expr(state, item, env))
            .collect::<Vec<_>>();
        if child_nodes.is_empty() {
            let provenance = Provenance::literal(state.span_for_node(tuple));
            return Ok(EvalStep::Ready(Value::tuple(Vec::new()).with_meta(
                ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue),
            )));
        }
        return Ok(wait_on_child_dependency(
            state,
            node_id,
            child_nodes[0],
            vec![ContinuationFrame::TupleJoin {
                node_id,
                parent_id,
                child_nodes,
                tuple_source: state.span_for_node(tuple),
            }],
        ));
    }
    continue_tuple_items(
        state,
        node_id,
        parent_id,
        Vec::new(),
        items.iter().map(|item| state.expr_handle(item)).collect(),
        env.clone(),
        state.span_for_node(tuple),
    )
}

pub(super) fn eval_interpolated_string(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    interpolated: &InterpolatedStringExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("interpolated string should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let string_source = state.span_for_node(interpolated);
    let parts = interpolated
        .parts()
        .map(|part| match part {
            StringPart::Text(token) => Ok(InterpolatedPart::Text(Arc::from(token.text()))),
            StringPart::Interpolation(interpolation) => {
                let expr = interpolation.expr().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "string interpolation has no embedded expression",
                    )
                })?;
                Ok(InterpolatedPart::Expr(state.expr_handle(&expr)))
            }
        })
        .collect::<Result<Vec<_>, EvalError>>()?;

    if state.queue_child_work {
        let expr_nodes = parts
            .iter()
            .filter_map(|part| match part {
                InterpolatedPart::Text(_) => None,
                InterpolatedPart::Expr(expr) => Some(queue_child_expr_handle(state, *expr, env)),
            })
            .collect::<Vec<_>>();
        if let Some(child_id) = expr_nodes.first().copied() {
            return Ok(wait_on_child_dependency(
                state,
                node_id,
                child_id,
                vec![ContinuationFrame::InterpolatedStringJoin {
                    node_id,
                    parent_id,
                    parts,
                    expr_nodes,
                    string_source,
                }],
            ));
        }
        return Ok(EvalStep::Ready(ready_interpolated_string_value(
            Vec::new(),
            string_source,
        )));
    }

    continue_interpolated_string_parts(
        state,
        node_id,
        parent_id,
        Vec::new(),
        parts,
        env.clone(),
        string_source,
    )
}

pub(super) fn eval_range(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    range: &RangeExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("range should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let range_source = state.span_for_node(range);
    let start = range.start();
    let end = range.end();
    let inclusive_end = range.is_inclusive();

    if state.queue_child_work {
        let start_child = start
            .as_ref()
            .map(|expr| queue_child_expr(state, expr, env));
        let end_child = end.as_ref().map(|expr| queue_child_expr(state, expr, env));
        if let Some(child_id) = start_child.or(end_child) {
            return Ok(wait_on_child_dependency(
                state,
                node_id,
                child_id,
                vec![ContinuationFrame::RangeJoin {
                    node_id,
                    parent_id,
                    start_child,
                    end_child,
                    range_source,
                    inclusive_end,
                }],
            ));
        }
        return Ok(EvalStep::Ready(ready_range_value(
            None,
            None,
            inclusive_end,
            range_source,
        )));
    }

    let start_handle = start.as_ref().map(|expr| state.expr_handle(expr));
    let end_handle = end.as_ref().map(|expr| state.expr_handle(expr));
    if let Some(start_handle) = start_handle {
        match eval_dependent_expr_handle(state, node_id, start_handle, env)? {
            EvalStep::Ready(start) => continue_range_end(
                state,
                node_id,
                parent_id,
                Some(start),
                end_handle,
                env.clone(),
                range_source,
                inclusive_end,
            ),
            EvalStep::Suspended(suspension) => Ok(extend_suspension(
                suspension,
                ContinuationFrame::RangeAfterStart {
                    node_id,
                    parent_id,
                    end: end_handle,
                    env: env.clone(),
                    range_source,
                    inclusive_end,
                },
            )),
        }
    } else {
        continue_range_end(
            state,
            node_id,
            parent_id,
            None,
            end_handle,
            env.clone(),
            range_source,
            inclusive_end,
        )
    }
}

pub(super) fn eval_map(
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

pub(super) fn eval_fail(
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

pub(super) fn user_fail_error(value: Value) -> EvalError {
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
        ValueRep::Tuple(_) => EvalError::new(EvalErrorKind::UserFail, "Tuple").with_detail(value),
        ValueRep::Range(_) => EvalError::new(EvalErrorKind::UserFail, "Range").with_detail(value),
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

pub(super) fn eval_if(
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

pub(super) fn eval_match(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    match_expr: &MatchExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("match expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let scrutinee = match_expr
        .syntax()
        .children()
        .find_map(Expr::cast)
        .ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "match expression has no scrutinee")
        })?;
    let arms = Arc::<[MatchArmPlan]>::from(
        match_expr
            .arms()
            .map(|arm| {
                let pattern = arm.pattern().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "match arm has no pattern")
                })?;
                let body = arm.expr().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "match arm has no body")
                })?;
                Ok(MatchArmPlan {
                    pattern: match_pattern_from_ast(state, &pattern)?,
                    body: state.expr_handle(&body),
                })
            })
            .collect::<Result<Vec<_>, EvalError>>()?,
    );
    let scrutinee = match eval_child_expr(state, &scrutinee, env)? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::MatchScrutinee {
                    node_id,
                    parent_id,
                    arms,
                    env: env.clone(),
                },
            ));
        }
    };
    continue_match_arms(state, node_id, parent_id, scrutinee, arms, env.clone())
}

pub(super) fn continue_match_arms(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    scrutinee: Value,
    arms: Arc<[MatchArmPlan]>,
    env: EvalEnv,
) -> Result<EvalStep, EvalError> {
    for arm in arms.iter() {
        let mut arm_env = env.clone();
        if match_pattern(state, &mut arm_env, &arm.pattern, scrutinee.clone())? {
            return match eval_dependent_expr_handle(state, node_id, arm.body, &mut arm_env)? {
                EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
                EvalStep::Suspended(suspension) => Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::FinishNode { node_id, parent_id },
                )),
            };
        }
    }
    Err(EvalError::new(
        EvalErrorKind::Internal,
        "match expression had no matching arm",
    ))
}

pub(super) fn eval_where(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    where_expr: &vx_parser::WhereExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("where expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let call = where_expr
        .callee()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "where expression has no callee"))?;
    let Expr::Call(call_expr) = call else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "where expression currently requires a call callee",
        ));
    };
    let keywords = where_expr.keywords().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            "where expression has no keyword record",
        )
    })?;
    let Expr::Record(record) = keywords else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "where expression currently requires a record keyword payload",
        ));
    };
    let mut keyword_bindings = Vec::new();
    let mut remaining_keyword_bindings = Vec::new();
    for field in record.fields() {
        let name_token = field.name().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "where keyword field is missing a name",
            )
        })?;
        let name = Arc::<str>::from(name_token.text().to_string());
        match field.value() {
            Some(expr) => remaining_keyword_bindings.push((name, state.expr_handle(&expr))),
            None => {
                let Some(value) = env.get(name_token.text()) else {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        format!(
                            "where binding {} is not available in scope",
                            name_token.text()
                        ),
                    ));
                };
                keyword_bindings.push((name, value.clone()));
            }
        }
    }
    continue_where_keyword_bindings(
        state,
        node_id,
        parent_id,
        &call_expr,
        keyword_bindings,
        remaining_keyword_bindings,
        env.clone(),
    )
}

pub(super) fn continue_where_keyword_bindings(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    call_expr: &CallExpr,
    mut keyword_bindings: Vec<(Arc<str>, Value)>,
    remaining_keyword_bindings: Vec<(Arc<str>, ExprHandle)>,
    mut env: EvalEnv,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_keyword_bindings.into_iter();
    while let Some((name, expr_handle)) = remaining.next() {
        match eval_dependent_expr_handle(state, node_id, expr_handle, &mut env)? {
            EvalStep::Ready(value) => keyword_bindings.push((name, value)),
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::WhereAfterKeyword {
                        node_id,
                        parent_id,
                        call: ExprHandle {
                            key: ProgramNodeHandle::for_node(state.current_source(), call_expr),
                        },
                        pending_keyword_name: name,
                        keyword_bindings,
                        remaining_keyword_bindings: remaining.collect(),
                        env,
                    },
                ));
            }
        }
    }
    eval_call_with_keyword_bindings(state, node_id, parent_id, call_expr, env, keyword_bindings)
}

fn eval_call_with_keyword_bindings(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    call: &CallExpr,
    env: EvalEnv,
    keyword_bindings: Vec<(Arc<str>, Value)>,
) -> Result<EvalStep, EvalError> {
    let callee = call
        .callee()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "call expression has no callee"))?;
    let call_source = state.span_for_node(call);
    let type_args = Arc::<[Ty]>::from(
        call.type_args()
            .into_iter()
            .flat_map(|args| args.types())
            .map(|ty| state.program.resolve_type_annotation(&ty))
            .collect::<Vec<_>>(),
    );
    let positional_args = call.positional_args().into_iter().collect::<Vec<_>>();
    let Some(callee_value) = immediate_call_callee_value(state, &callee, &env)? else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "where call callee must resolve immediately for now",
        ));
    };
    let callee_value = apply_where_keyword_bindings(callee_value, keyword_bindings);
    if state.queue_child_work && !positional_args.is_empty() {
        let child_nodes = positional_args
            .iter()
            .map(|arg| queue_child_expr(state, arg, &env))
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
        env,
    )
}

fn apply_where_keyword_bindings(callee: Value, keyword_bindings: Vec<(Arc<str>, Value)>) -> Value {
    let ValueRep::Function(function) = &callee.rep else {
        return callee;
    };
    let mut captures = function.captures.iter().cloned().collect::<Vec<_>>();
    for (name, value) in keyword_bindings {
        match captures.iter_mut().find(|(existing, _)| existing == &name) {
            Some((_, existing_value)) => *existing_value = value,
            None => captures.push((name, value)),
        }
    }
    Value::function(FunctionRef::new(
        function.source_id.clone(),
        function.name.clone(),
        Arc::clone(&function.parameters),
        false,
        function.body,
        Arc::from(captures),
        function.self_name.clone(),
    ))
    .with_meta(callee.meta)
}

pub(super) fn eval_field(
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
            let field_source = state.span_for_node(field);
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::FieldReceiver {
                    node_id,
                    parent_id,
                    field_name,
                    field_source,
                },
            ));
        }
    };
    project_field_or_method_value(
        state,
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
        state.span_for_node(field),
    )
    .map(EvalStep::Ready)
}

pub(super) fn eval_binary(
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

pub(super) fn eval_prefix(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    prefix: &PrefixExpr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("prefix expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let operand = prefix.expr().ok_or_else(|| {
        EvalError::new(EvalErrorKind::Internal, "prefix expression has no operand")
    })?;
    let operator = prefix
        .operator_token()
        .map(|token| token.kind())
        .ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "prefix expression has no operator")
        })?;
    match eval_child_expr(state, &operand, env)? {
        EvalStep::Ready(value) => apply_prefix_operator(operator, value).map(EvalStep::Ready),
        EvalStep::Suspended(suspension) => Ok(extend_suspension(
            suspension,
            ContinuationFrame::PrefixOperand {
                node_id,
                parent_id,
                operator,
            },
        )),
    }
}

pub(super) fn continue_block_statements(
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
                let pattern = let_stmt.pattern().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no pattern")
                })?;
                let let_pattern = state.with_source(statement_handle.key.source(), |state| {
                    let_pattern_from_ast(state, &pattern)
                })?;
                let expr = let_stmt.expr().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no value")
                })?;
                match state.with_source(statement_handle.key.source(), |state| {
                    eval_dependent_expr(state, node_id, &expr, &mut scope)
                })? {
                    EvalStep::Ready(value) => {
                        bind_let_pattern(&mut scope, &let_pattern, value)?;
                    }
                    EvalStep::Suspended(suspension) => {
                        return Ok(extend_suspension(
                            suspension,
                            ContinuationFrame::BlockAfterLet {
                                node_id,
                                parent_id,
                                scope,
                                last,
                                pattern: let_pattern,
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

pub(super) fn continue_array_items(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut items: Vec<Value>,
    remaining_items: Vec<ArrayEvalItem>,
    mut env: EvalEnv,
    array_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_items.into_iter();
    while let Some(item_handle) = remaining.next() {
        let (item_handle, spread) = match item_handle {
            ArrayEvalItem::Expr(handle) => (handle, false),
            ArrayEvalItem::Spread(handle) => (handle, true),
        };
        match eval_dependent_expr_handle(state, node_id, item_handle, &mut env)? {
            EvalStep::Ready(value) => append_array_item(&mut items, value, spread)?,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayAfterItem {
                        node_id,
                        parent_id,
                        items,
                        spread,
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

pub(super) fn continue_tuple_items(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut items: Vec<Value>,
    remaining_items: Vec<ExprHandle>,
    mut env: EvalEnv,
    tuple_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_items.into_iter();
    while let Some(item_handle) = remaining.next() {
        match eval_dependent_expr_handle(state, node_id, item_handle, &mut env)? {
            EvalStep::Ready(value) => items.push(value),
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::TupleAfterItem {
                        node_id,
                        parent_id,
                        items,
                        remaining_items: remaining.collect(),
                        env,
                        tuple_source,
                    },
                ));
            }
        }
    }
    let provenance = Provenance::literal(tuple_source);
    let meta = aggregate_child_meta(provenance, items.iter());
    Ok(EvalStep::Ready(Value::tuple(items).with_meta(meta)))
}

pub(super) fn continue_range_end(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    start: Option<Value>,
    end: Option<ExprHandle>,
    mut env: EvalEnv,
    range_source: SourceSpan,
    inclusive_end: bool,
) -> Result<EvalStep, EvalError> {
    let end = match end {
        Some(end_handle) => match eval_dependent_expr_handle(state, node_id, end_handle, &mut env)?
        {
            EvalStep::Ready(end) => Some(end),
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::RangeAfterEnd {
                        node_id,
                        parent_id,
                        start,
                        range_source,
                        inclusive_end,
                    },
                ));
            }
        },
        None => None,
    };
    Ok(EvalStep::Ready(build_range_value(
        start,
        end,
        inclusive_end,
        range_source,
    )?))
}

pub(super) fn continue_interpolated_string_parts(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut segments: Vec<String>,
    remaining_parts: Vec<InterpolatedPart>,
    mut env: EvalEnv,
    string_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_parts.into_iter();
    while let Some(part) = remaining.next() {
        match part {
            InterpolatedPart::Text(text) => segments.push(text.to_string()),
            InterpolatedPart::Expr(expr) => {
                match eval_dependent_expr_handle(state, node_id, expr, &mut env)? {
                    EvalStep::Ready(value) => segments.push(interpolation_segment_text(&value)?),
                    EvalStep::Suspended(suspension) => {
                        return Ok(extend_suspension(
                            suspension,
                            ContinuationFrame::InterpolatedStringAfterExpr {
                                node_id,
                                parent_id,
                                segments,
                                remaining_parts: remaining.collect(),
                                env,
                                string_source,
                            },
                        ));
                    }
                }
            }
        }
    }
    Ok(EvalStep::Ready(ready_interpolated_string_value(
        segments,
        string_source,
    )))
}

fn build_range_value(
    start: Option<Value>,
    end: Option<Value>,
    inclusive_end: bool,
    range_source: SourceSpan,
) -> Result<Value, EvalError> {
    let start = range_bound_as_u64(start.as_ref(), "range start")?;
    let end = range_bound_as_u64(end.as_ref(), "range end")?;
    Ok(ready_range_value(start, end, inclusive_end, range_source))
}

fn range_bound_as_u64(value: Option<&Value>, label: &str) -> Result<Option<u64>, EvalError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let ValueRep::U64(bound) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{label} must evaluate to U64"),
        ));
    };
    Ok(Some(*bound))
}

fn ready_range_value(
    start: Option<u64>,
    end: Option<u64>,
    inclusive_end: bool,
    range_source: SourceSpan,
) -> Value {
    let range = Value::range(start, end, inclusive_end);
    let provenance = Provenance::literal(range_source);
    range.with_meta(ValueMeta::new(
        provenance,
        ReuseClass::Stable,
        CrossingPolicy::ByValue,
    ))
}

fn interpolation_segment_text(value: &Value) -> Result<String, EvalError> {
    match &value.rep {
        ValueRep::String(text) => Ok(text.to_string()),
        ValueRep::Bool(value) => Ok(value.to_string()),
        ValueRep::U64(value) => Ok(value.to_string()),
        other => Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "interpolated string expressions must evaluate to String/Bool/U64, got {other:?}"
            ),
        )),
    }
}

fn ready_interpolated_string_value(segments: Vec<String>, string_source: SourceSpan) -> Value {
    let value = Value::string(segments.concat());
    value.with_meta(ValueMeta::new(
        Provenance::literal(string_source),
        ReuseClass::Stable,
        CrossingPolicy::ByValue,
    ))
}

pub(super) fn let_pattern_from_ast(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    pattern: &Pattern,
) -> Result<LetPattern, EvalError> {
    match pattern {
        Pattern::Binding(binding) => {
            let name_token = binding.name().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "binding pattern has no name")
            })?;
            let name = name_token.text().to_string();
            Ok(LetPattern::Binding {
                name,
                source: state.span_for_token(&name_token),
            })
        }
        Pattern::Wildcard(_) => Ok(LetPattern::Wildcard),
        Pattern::Paren(paren) => {
            let inner = paren.inner().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "paren pattern has no inner pattern",
                )
            })?;
            let_pattern_from_ast(state, &inner)
        }
        Pattern::Unit(_) => Ok(LetPattern::Unit),
        Pattern::Tuple(tuple) => Ok(LetPattern::Tuple(
            tuple
                .items()
                .map(|item| let_pattern_from_ast(state, &item))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported let pattern: {other:?}"),
        )),
    }
}

pub(super) fn bind_let_pattern(
    scope: &mut EvalEnv,
    pattern: &LetPattern,
    value: Value,
) -> Result<(), EvalError> {
    match pattern {
        LetPattern::Binding { name, source } => {
            env_mut(scope).insert(
                name.clone(),
                value.with_provenance(Provenance::binding(source.clone(), name.clone())),
            );
            Ok(())
        }
        LetPattern::Wildcard => Ok(()),
        LetPattern::Unit => match value.rep {
            ValueRep::Unit => Ok(()),
            _ => Err(EvalError::new(
                EvalErrorKind::TypeError,
                "unit pattern requires unit value",
            )),
        },
        LetPattern::Tuple(patterns) => {
            let ValueRep::Tuple(items) = value.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "tuple pattern requires tuple value",
                ));
            };
            if patterns.len() != items.len() {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!(
                        "tuple pattern expects {} element(s), got {}",
                        patterns.len(),
                        items.len()
                    ),
                ));
            }
            for (pattern, item) in patterns.iter().zip(items.iter().cloned()) {
                bind_let_pattern(scope, pattern, item)?;
            }
            Ok(())
        }
    }
}

pub(super) fn match_pattern(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    scope: &mut EvalEnv,
    pattern: &MatchPattern,
    value: Value,
) -> Result<bool, EvalError> {
    match pattern {
        MatchPattern::Binding { name, source } => {
            if name == "_" {
                return Ok(true);
            }
            env_mut(scope).insert(
                name.clone(),
                value.with_provenance(Provenance::binding(source.clone(), name.clone())),
            );
            Ok(true)
        }
        MatchPattern::Wildcard => Ok(true),
        MatchPattern::Unit => Ok(matches!(value.rep, ValueRep::Unit)),
        MatchPattern::Tuple(patterns) => {
            let ValueRep::Tuple(items) = &value.rep else {
                return Ok(false);
            };
            if patterns.len() != items.len() {
                return Ok(false);
            }
            for (pattern, item) in patterns.iter().zip(items.iter().cloned()) {
                if !match_pattern(state, scope, pattern, item)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        MatchPattern::Literal(expected) => values_equal(&expected.rep, &value.rep),
        MatchPattern::Variant { name, fields } => {
            match_variant_pattern(state, scope, name, fields.as_deref(), value)
        }
        MatchPattern::Struct(fields) => {
            let ValueRep::Struct(struct_value) = &value.rep else {
                return Ok(false);
            };
            match_struct_pattern_fields(state, scope, fields, struct_value)
        }
        MatchPattern::At {
            binding_name,
            binding_source,
            pattern,
        } => {
            env_mut(scope).insert(
                binding_name.clone(),
                value.clone().with_provenance(Provenance::binding(
                    binding_source.clone(),
                    binding_name.clone(),
                )),
            );
            match_pattern(state, scope, pattern, value)
        }
        MatchPattern::Or(alternatives) => {
            for alternative in alternatives {
                let mut alternative_scope = scope.clone();
                if match_pattern(state, &mut alternative_scope, alternative, value.clone())? {
                    *scope = alternative_scope;
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn match_variant_pattern(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    scope: &mut EvalEnv,
    variant_name: &Arc<str>,
    fields: Option<&[MatchFieldPattern]>,
    value: Value,
) -> Result<bool, EvalError> {
    let ValueRep::Struct(struct_value) = &value.rep else {
        return Ok(false);
    };
    if struct_value.type_name.as_ref() != variant_name.as_ref() {
        return Ok(false);
    }
    match fields {
        Some(fields) => match_struct_pattern_fields(state, scope, fields, struct_value),
        None => Ok(true),
    }
}

fn match_struct_pattern_fields(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    scope: &mut EvalEnv,
    fields: &[MatchFieldPattern],
    struct_value: &StructValue,
) -> Result<bool, EvalError> {
    for field in fields {
        let name = field.name.as_str();
        let Some(value) = struct_value
            .fields
            .iter()
            .find_map(|(field_name, value)| (field_name.as_ref() == name).then_some(value))
        else {
            return Ok(false);
        };
        if let Some(pattern) = &field.pattern {
            if !match_pattern(state, scope, pattern, value.clone())? {
                return Ok(false);
            }
        } else {
            env_mut(scope).insert(
                field.name.clone(),
                value.clone().with_provenance(Provenance::binding(
                    field.source.clone(),
                    field.name.clone(),
                )),
            );
        }
    }
    Ok(true)
}

fn match_pattern_from_ast(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    pattern: &Pattern,
) -> Result<MatchPattern, EvalError> {
    match pattern {
        Pattern::Binding(binding) => {
            let name_token = binding.name().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "binding pattern has no name")
            })?;
            Ok(MatchPattern::Binding {
                name: name_token.text().to_string(),
                source: state.span_for_token(&name_token),
            })
        }
        Pattern::Wildcard(_) => Ok(MatchPattern::Wildcard),
        Pattern::Literal(literal) => {
            literal_pattern_value(state, literal).map(MatchPattern::Literal)
        }
        Pattern::Tuple(tuple) => Ok(MatchPattern::Tuple(
            tuple
                .items()
                .map(|item| match_pattern_from_ast(state, &item))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Pattern::Struct(struct_pattern) => Ok(MatchPattern::Struct(
            struct_pattern
                .fields()
                .map(|field| match_field_pattern_from_ast(state, &field))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Pattern::Paren(paren) => {
            let inner = paren.inner().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "paren pattern has no inner pattern",
                )
            })?;
            match_pattern_from_ast(state, &inner)
        }
        Pattern::Unit(_) => Ok(MatchPattern::Unit),
        Pattern::Variant(variant) => Ok(MatchPattern::Variant {
            name: Arc::from(
                variant
                    .segments()
                    .last()
                    .ok_or_else(|| {
                        EvalError::new(EvalErrorKind::Internal, "variant pattern has no name")
                    })?
                    .text(),
            ),
            fields: variant
                .fields()
                .map(|fields| {
                    fields
                        .fields()
                        .map(|field| match_field_pattern_from_ast(state, &field))
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
        }),
        Pattern::At(at) => {
            let binding_name = at.binding_name().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "at pattern has no binding name")
            })?;
            let pattern = at.pattern().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "at pattern has no nested pattern")
            })?;
            Ok(MatchPattern::At {
                binding_name: binding_name.text().to_string(),
                binding_source: state.span_for_token(&binding_name),
                pattern: Box::new(match_pattern_from_ast(state, &pattern)?),
            })
        }
        Pattern::Or(or) => Ok(MatchPattern::Or(
            or.patterns()
                .map(|pattern| match_pattern_from_ast(state, &pattern))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported match pattern: {other:?}"),
        )),
    }
}

fn match_field_pattern_from_ast(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    field: &vx_parser::StructPatternField,
) -> Result<MatchFieldPattern, EvalError> {
    let name_token = field.name().ok_or_else(|| {
        EvalError::new(EvalErrorKind::Internal, "struct pattern field has no name")
    })?;
    Ok(MatchFieldPattern {
        name: name_token.text().to_string(),
        source: state.span_for_token(&name_token),
        pattern: field
            .pattern()
            .map(|pattern| match_pattern_from_ast(state, &pattern).map(Box::new))
            .transpose()?,
    })
}

fn literal_pattern_value(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    literal: &vx_parser::LiteralPattern,
) -> Result<Value, EvalError> {
    let token = literal
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "literal pattern has no token"))?;
    let source = state.span_for_token(&token);
    match token.kind() {
        SyntaxKind::INT => {
            let value = token.text().parse::<u64>().map_err(|source| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("invalid integer literal {}: {source}", token.text()),
                )
            })?;
            Ok(Value::u64(value).with_provenance(Provenance::literal(source)))
        }
        SyntaxKind::TRUE_KW => Ok(Value::bool(true).with_provenance(Provenance::literal(source))),
        SyntaxKind::FALSE_KW => Ok(Value::bool(false).with_provenance(Provenance::literal(source))),
        SyntaxKind::STRING => {
            let text = token.text();
            Ok(
                Value::string(&text[1..text.len() - 1])
                    .with_provenance(Provenance::literal(source)),
            )
        }
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported literal pattern token: {other:?}"),
        )),
    }
}

pub(super) fn queue_child_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &EvalEnv,
) -> EvalNodeId {
    queue_child_expr_handle(state, state.expr_handle(expr), env)
}

pub(super) fn queue_child_expr_handle(
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

pub(super) fn wait_on_child_dependency(
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

pub(super) fn continue_map_entries(
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

pub(super) fn continue_struct_fields(
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

pub(super) fn project_struct_field(
    receiver: Value,
    field_name: String,
) -> Result<Value, EvalError> {
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

pub(super) fn expr_kind(expr: &Expr) -> EvalExprKind {
    match expr {
        Expr::Block(_) => EvalExprKind::Block,
        Expr::Literal(_) => EvalExprKind::Literal,
        Expr::Paren(_) => EvalExprKind::Paren,
        Expr::Closure(_) => EvalExprKind::Closure,
        Expr::NameRef(_) => EvalExprKind::NameRef,
        Expr::Call(_) => EvalExprKind::Call,
        Expr::Array(_) => EvalExprKind::Array,
        Expr::Tuple(_) => EvalExprKind::Tuple,
        Expr::Range(_) => EvalExprKind::Range,
        Expr::Map(_) => EvalExprKind::Map,
        Expr::Fail(_) => EvalExprKind::Fail,
        Expr::If(_) => EvalExprKind::If,
        Expr::Match(_) => EvalExprKind::Match,
        Expr::Where(_) => EvalExprKind::Where,
        Expr::Prefix(_) => EvalExprKind::Prefix,
        Expr::Binary(_) => EvalExprKind::Binary,
        Expr::InterpolatedString(_) => EvalExprKind::InterpolatedString,
        Expr::StructLiteral(_) => EvalExprKind::StructLiteral,
        Expr::Field(_) => EvalExprKind::Field,
        _ => EvalExprKind::Unsupported,
    }
}

pub(super) fn binary_operator(binary: &BinaryExpr) -> Result<SyntaxKind, EvalError> {
    binary
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .map(|token| token.kind())
        .find(|kind| {
            matches!(
                kind,
                SyntaxKind::AMP2 | SyntaxKind::PIPE2 | SyntaxKind::EQ2 | SyntaxKind::BANG_EQ
            )
        })
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "binary expression has no operator"))
}

pub(super) fn continue_binary_rhs(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    lhs: Value,
    operator: SyntaxKind,
    rhs: ExprHandle,
    mut env: EvalEnv,
) -> Result<EvalStep, EvalError> {
    if let Some(short_circuit) = short_circuit_binary(&lhs, operator)? {
        return Ok(EvalStep::Ready(short_circuit));
    }

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

pub(super) fn apply_binary_operator(
    lhs: Value,
    operator: SyntaxKind,
    rhs: Value,
) -> Result<Value, EvalError> {
    let equals = values_equal(&lhs.rep, &rhs.rep)?;
    match operator {
        SyntaxKind::AMP2 => {
            let ValueRep::Bool(lhs) = lhs.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "left operand of && must be Bool",
                ));
            };
            let ValueRep::Bool(rhs) = rhs.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "right operand of && must be Bool",
                ));
            };
            Ok(Value::bool(lhs && rhs))
        }
        SyntaxKind::PIPE2 => {
            let ValueRep::Bool(lhs) = lhs.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "left operand of || must be Bool",
                ));
            };
            let ValueRep::Bool(rhs) = rhs.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "right operand of || must be Bool",
                ));
            };
            Ok(Value::bool(lhs || rhs))
        }
        SyntaxKind::EQ2 => Ok(Value::bool(equals)),
        SyntaxKind::BANG_EQ => Ok(Value::bool(!equals)),
        _ => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("binary operator {operator:?} is not supported yet"),
        )),
    }
}

fn short_circuit_binary(lhs: &Value, operator: SyntaxKind) -> Result<Option<Value>, EvalError> {
    match operator {
        SyntaxKind::AMP2 => {
            let ValueRep::Bool(lhs) = &lhs.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "left operand of && must be Bool",
                ));
            };
            Ok((!*lhs).then(|| Value::bool(false)))
        }
        SyntaxKind::PIPE2 => {
            let ValueRep::Bool(lhs) = &lhs.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "left operand of || must be Bool",
                ));
            };
            Ok((*lhs).then(|| Value::bool(true)))
        }
        _ => Ok(None),
    }
}

pub(super) fn apply_prefix_operator(
    operator: SyntaxKind,
    operand: Value,
) -> Result<Value, EvalError> {
    match operator {
        SyntaxKind::BANG => {
            let ValueRep::Bool(value) = operand.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "operand of ! must be Bool",
                ));
            };
            Ok(Value::bool(!value).with_meta(operand.meta))
        }
        _ => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("prefix operator {operator:?} is not supported yet"),
        )),
    }
}

pub(super) fn values_equal(lhs: &ValueRep, rhs: &ValueRep) -> Result<bool, EvalError> {
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
        (ValueRep::Tuple(lhs), ValueRep::Tuple(rhs)) => {
            if lhs.len() != rhs.len() {
                return Ok(false);
            }
            lhs.iter()
                .zip(rhs.iter())
                .try_fold(true, |equal, (lhs, rhs)| {
                    Ok(equal && values_equal(&lhs.rep, &rhs.rep)?)
                })
        }
        (ValueRep::Range(lhs), ValueRep::Range(rhs)) => Ok(lhs == rhs),
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

pub(super) fn join_keyed_input(
    existing: &Value,
    incoming: &Value,
) -> Result<(Value, bool), EvalError> {
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

pub(super) fn merge_keyed_node(
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

pub(super) fn join_reuse(left: ReuseClass, right: ReuseClass) -> ReuseClass {
    match (left, right) {
        (ReuseClass::Live, _) | (_, ReuseClass::Live) => ReuseClass::Live,
        (ReuseClass::Observed, _) | (_, ReuseClass::Observed) => ReuseClass::Observed,
        (ReuseClass::Stable, ReuseClass::Stable) => ReuseClass::Stable,
    }
}

pub(super) fn eval_literal(
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
