use super::*;
use crate::program::{
    ArrayPlan, ArrayPlanItem, BytecodeBody, BytecodeOp, CallCalleePlan, CallCalleePlanKind,
    ClosurePlan, DirectCallCalleePlan, DirectCallCalleePlanKind, ExprExecKind, FieldPlan,
    InterpolatedStringPlan, InterpolatedStringPlanPart, LiteralPlan, MapPlan, MatchPlan,
    NameRefPlan, PathLiteralPlan, RangePlan, ReadyPlanKind, StructLiteralPlan,
    StructLiteralPlanPart, TuplePlan, WherePlan,
};
use crate::value::compare_value_reps;

mod builtins;
pub(in crate::engine) use builtins::*;

mod resume;
pub(in crate::engine) use resume::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedBodyRef {
    root_expr_id: ExprId,
    kind: EvalExprKind,
    body: BytecodeBodyId,
}

pub(super) struct PreparedEntry {
    pub(super) body: PreparedBodyRef,
    pub(super) env: EvalEnv,
    pub(super) locals: Vec<Option<Value>>,
}

impl PreparedBodyRef {
    pub(super) fn from_body_ref(
        program: &CompiledProgram,
        body: ExprBodyRef,
    ) -> Result<Self, EvalError> {
        let body = if body.plan().kind() == ExprExecKind::Expr {
            program.resolve_expr_body_ref(body)?
        } else {
            body
        };
        Self::from_resolved_body_ref(program, body)
    }

    fn from_resolved_body_ref(
        program: &CompiledProgram,
        body: ExprBodyRef,
    ) -> Result<Self, EvalError> {
        let bytecode = program
            .resolve_expr_bytecode_body(body.expr_id())?
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("prepared body {:?} has no bytecode body", body.expr_id()),
                )
            })?;
        Ok(Self {
            root_expr_id: body.expr_id(),
            kind: body.plan().trace_kind(),
            body: bytecode,
        })
    }

    pub(super) fn root_expr_id(&self) -> ExprId {
        self.root_expr_id
    }
}

fn empty_bytecode_locals(body: &BytecodeBody) -> Vec<Option<Value>> {
    vec![None; body.local_count]
}

fn set_bytecode_local(
    body: &BytecodeBody,
    locals: &mut [Option<Value>],
    name: &str,
    value: &Value,
) {
    if let Some(&slot) = body.slot_lookup.get(name) {
        locals[slot as usize] = Some(value.clone());
    }
}

fn init_bytecode_locals_from_scope(body: &BytecodeBody, scope: &EvalEnv) -> Vec<Option<Value>> {
    let mut locals = empty_bytecode_locals(body);
    for (slot, local) in body.local_slots.iter().enumerate() {
        if local.prefill_from_scope {
            locals[slot] = scope.get(local.name.as_ref()).cloned();
        }
    }
    locals
}

fn prepared_entry_from_scope(
    program: &CompiledProgram,
    body: PreparedBodyRef,
    env: EvalEnv,
) -> Result<PreparedEntry, EvalError> {
    let bytecode_body = program.resolve_bytecode_body(body.body)?;
    Ok(PreparedEntry {
        body,
        locals: init_bytecode_locals_from_scope(bytecode_body, &env),
        env,
    })
}

fn init_bytecode_locals_from_parent(
    program: &CompiledProgram,
    body: &BytecodeBody,
    parent: &BytecodeState,
) -> Result<Vec<Option<Value>>, EvalError> {
    let parent_body = program.resolve_bytecode_body(parent.body)?;
    let mut locals = empty_bytecode_locals(body);
    for (slot, local) in body.local_slots.iter().enumerate() {
        if !local.prefill_from_scope {
            continue;
        }
        if let Some(&parent_slot) = parent_body.slot_lookup.get(local.name.as_ref())
            && let Some(Some(value)) = parent.locals.get(parent_slot as usize)
        {
            locals[slot] = Some(value.clone());
            continue;
        }
        locals[slot] = parent.scope.get(local.name.as_ref()).cloned();
    }
    Ok(locals)
}

fn prepared_entry_from_bytecode(
    program: &CompiledProgram,
    body: ExprBodyRef,
    parent: &BytecodeState,
) -> Result<PreparedEntry, EvalError> {
    let body = PreparedBodyRef::from_body_ref(program, body)?;
    let bytecode_body = program.resolve_bytecode_body(body.body)?;
    Ok(PreparedEntry {
        body,
        locals: init_bytecode_locals_from_parent(program, bytecode_body, parent)?,
        env: parent.scope.clone(),
    })
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

fn dollar_placeholder_index(text: &str) -> Option<usize> {
    if text == "$" {
        return Some(0);
    }
    text.strip_prefix('$')?.parse().ok()
}

fn compile_adapter_parameters(
    program: &CompiledProgram,
    expr: &Expr,
    fallback: ProgramNodeHandle,
) -> Result<Option<Arc<[CompiledParameter]>>, EvalError> {
    let mut names_by_index: Vec<Option<ProgramNodeHandle>> = Vec::new();
    collect_adapter_parameters(program, expr, &mut names_by_index)?;
    if names_by_index.is_empty() {
        return Ok(None);
    }
    Ok(Some(Arc::from(
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
    )))
}

fn collect_adapter_parameters(
    program: &CompiledProgram,
    expr: &Expr,
    names_by_index: &mut Vec<Option<ProgramNodeHandle>>,
) -> Result<(), EvalError> {
    match expr.kind() {
        ExprKind::NameRef(name_ref) => {
            collect_adapter_parameters_from_name_ref(name_ref, names_by_index)?;
        }
        ExprKind::Block(block) => {
            for step in program.resolve_block_body(block.body)?.steps.iter() {
                collect_adapter_parameters_from_block_step(program, step, names_by_index)?;
            }
        }
        ExprKind::Paren(paren) => {
            if let Some(inner) = paren.expr {
                collect_adapter_parameters(program, program.resolve_expr(inner)?, names_by_index)?;
            }
        }
        ExprKind::Closure(closure) => {
            if let Some(body) = closure.body {
                collect_adapter_parameters(program, program.resolve_expr(body)?, names_by_index)?;
            }
        }
        ExprKind::Call(call) => {
            collect_adapter_parameters_from_call(program, call, names_by_index)?;
        }
        ExprKind::Array(array) => {
            for item in array.items.iter() {
                let handle = match item {
                    ArrayItem::Expr(expr) | ArrayItem::Spread(expr) => *expr,
                };
                collect_adapter_parameters(program, program.resolve_expr(handle)?, names_by_index)?;
            }
        }
        ExprKind::Tuple(tuple) => {
            for item in tuple.items.iter().copied() {
                collect_adapter_parameters(program, program.resolve_expr(item)?, names_by_index)?;
            }
        }
        ExprKind::Range(range) => {
            if let Some(start) = range.start {
                collect_adapter_parameters(program, program.resolve_expr(start)?, names_by_index)?;
            }
            if let Some(end) = range.end {
                collect_adapter_parameters(program, program.resolve_expr(end)?, names_by_index)?;
            }
        }
        ExprKind::Map(map) => {
            for (key, value) in map.entries.iter().copied() {
                collect_adapter_parameters(program, program.resolve_expr(key)?, names_by_index)?;
                collect_adapter_parameters(program, program.resolve_expr(value)?, names_by_index)?;
            }
        }
        ExprKind::Fail(fail) => {
            if let Some(message) = fail.message {
                collect_adapter_parameters(
                    program,
                    program.resolve_expr(message)?,
                    names_by_index,
                )?;
            }
        }
        ExprKind::Fallible(fallible) => {
            if let Some(inner) = fallible.expr {
                collect_adapter_parameters(program, program.resolve_expr(inner)?, names_by_index)?;
            }
        }
        ExprKind::If(if_expr) => {
            for handle in [if_expr.condition, if_expr.then_branch, if_expr.else_branch]
                .into_iter()
                .flatten()
            {
                collect_adapter_parameters(program, program.resolve_expr(handle)?, names_by_index)?;
            }
        }
        ExprKind::Match(match_expr) => {
            if let Some(scrutinee) = match_expr.scrutinee {
                collect_adapter_parameters(
                    program,
                    program.resolve_expr(scrutinee)?,
                    names_by_index,
                )?;
            }
            for arm in match_expr.arms.iter() {
                collect_adapter_parameters(
                    program,
                    program.resolve_expr(arm.body.expr_id())?,
                    names_by_index,
                )?;
            }
        }
        ExprKind::Where(where_expr) => {
            if let Some(call) = where_expr.call.as_ref() {
                collect_adapter_parameters_from_call(program, call, names_by_index)?;
            }
            for binding in where_expr.keyword_bindings.iter() {
                if let Some(expr) = binding.expr {
                    collect_adapter_parameters(
                        program,
                        program.resolve_expr(expr)?,
                        names_by_index,
                    )?;
                }
            }
        }
        ExprKind::Prefix(prefix) => {
            if let Some(inner) = prefix.expr {
                collect_adapter_parameters(program, program.resolve_expr(inner)?, names_by_index)?;
            }
        }
        ExprKind::Binary(binary) => {
            for handle in [binary.lhs, binary.rhs].into_iter().flatten() {
                collect_adapter_parameters(program, program.resolve_expr(handle)?, names_by_index)?;
            }
        }
        ExprKind::InterpolatedString(interpolated) => {
            for part in interpolated.parts.iter() {
                let StringPart::Interpolation(expr) = part else {
                    continue;
                };
                collect_adapter_parameters(program, program.resolve_expr(*expr)?, names_by_index)?;
            }
        }
        ExprKind::StructLiteral(struct_literal) => {
            for part in struct_literal.parts.iter() {
                match part {
                    crate::program::StructLiteralPart::Field { expr, .. } => {
                        if let Some(expr) = expr {
                            collect_adapter_parameters(
                                program,
                                program.resolve_expr(*expr)?,
                                names_by_index,
                            )?;
                        }
                    }
                    crate::program::StructLiteralPart::Spread(expr) => {
                        collect_adapter_parameters(
                            program,
                            program.resolve_expr(*expr)?,
                            names_by_index,
                        )?;
                    }
                }
            }
        }
        ExprKind::Field(field) => {
            if let Some(receiver) = field.receiver {
                collect_adapter_parameters(
                    program,
                    program.resolve_expr(receiver)?,
                    names_by_index,
                )?;
            }
        }
        ExprKind::Literal(_) | ExprKind::PathLiteral(_) | ExprKind::Unsupported { .. } => {}
    }
    Ok(())
}

fn collect_adapter_parameters_from_call(
    program: &CompiledProgram,
    call: &CallExpr,
    names_by_index: &mut Vec<Option<ProgramNodeHandle>>,
) -> Result<(), EvalError> {
    if let Some(callee) = &call.callee {
        match callee {
            CallCallee::NameRef(name_ref) => {
                collect_adapter_parameters_from_name_ref(name_ref, names_by_index)?;
            }
            CallCallee::ResolvedFunction { .. }
            | CallCallee::VariantConstructor { .. }
            | CallCallee::NewtypeConstructor { .. }
            | CallCallee::StaticMember { .. } => {}
            CallCallee::Field(field) => {
                collect_adapter_parameters_from_field(program, field, names_by_index)?;
            }
            CallCallee::Expr(callee) => {
                collect_adapter_parameters(
                    program,
                    program.resolve_expr(*callee)?,
                    names_by_index,
                )?;
            }
        }
    }
    for arg in call.positional_args.iter().copied() {
        collect_adapter_parameters(program, program.resolve_expr(arg)?, names_by_index)?;
    }
    Ok(())
}

fn collect_adapter_parameters_from_name_ref(
    name_ref: &NameRefExpr,
    names_by_index: &mut Vec<Option<ProgramNodeHandle>>,
) -> Result<(), EvalError> {
    let Some(token) = &name_ref.token else {
        return Ok(());
    };
    if token.kind() != SyntaxKind::DOLLAR_IDENT {
        return Ok(());
    }
    let Some(index) = dollar_placeholder_index(token.text()) else {
        return Ok(());
    };
    if names_by_index.len() <= index {
        names_by_index.resize(index + 1, None);
    }
    names_by_index[index] = Some(token.handle());
    Ok(())
}

fn collect_adapter_parameters_from_field(
    program: &CompiledProgram,
    field: &FieldExpr,
    names_by_index: &mut Vec<Option<ProgramNodeHandle>>,
) -> Result<(), EvalError> {
    if let Some(receiver) = field.receiver {
        collect_adapter_parameters(program, program.resolve_expr(receiver)?, names_by_index)?;
    }
    Ok(())
}

fn collect_adapter_parameters_from_block_step(
    program: &CompiledProgram,
    step: &BlockStep,
    names_by_index: &mut Vec<Option<ProgramNodeHandle>>,
) -> Result<(), EvalError> {
    match step {
        BlockStep::Let { expr, .. } | BlockStep::Expr(expr) => {
            collect_adapter_parameters(program, program.resolve_expr_id(*expr)?, names_by_index)?;
        }
        BlockStep::FnDecl(function) => {
            if let Some(body) = function.compiled.body {
                collect_adapter_parameters(
                    program,
                    program.resolve_expr_id(body)?,
                    names_by_index,
                )?;
            }
        }
    }
    Ok(())
}

fn builtin_adapter_arg_index(function_name: &str, arg_index: usize) -> bool {
    match function_name {
        "Array.any" | "Array.find" | "Array.filter" | "Array.map" | "Array.flat_map"
        | "Array.filter_map" | "Map.map" | "Stream.filter" | "Stream.map" | "Stream.flat_map" => {
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
    arg: ExprBodyRef,
    env: &EvalEnv,
) -> Result<Option<Value>, EvalError> {
    let ValueRep::Function(function) = &callee.rep else {
        return Ok(None);
    };
    if !builtin_adapter_arg_index(function.name.as_ref(), arg_index) {
        return Ok(None);
    }
    let arg_id = arg.expr_id();
    let expr = state.program.resolve_expr_id(arg_id)?;
    let Some(parameters) = compile_adapter_parameters(state.program, expr, expr.handle())? else {
        return Ok(None);
    };
    let function = FunctionRef::new(
        state.program.source_id(arg_id.source()),
        Arc::from("<adapter>"),
        parameters,
        false,
        Some(arg),
        captures_from_scope(env),
        None,
    );
    Ok(Some(Value::function(function).with_provenance(
        Provenance::literal(state.program.span_for_expr(expr)),
    )))
}

pub(super) fn bind_named_function_decl(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    function: &crate::program::FunctionStmt,
    scope: &EvalEnv,
) -> Result<(String, SourceSpan, Value), EvalError> {
    let name = function.name.to_string();
    let name_source = state.program.span_for_handle(function.compiled.definition);
    let compiled_name = function.name.clone();
    let compiled = function.compiled.clone();
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

fn eval_closure_body(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    handle: ProgramNodeHandle,
    parameters: Arc<[CompiledParameter]>,
    body: Option<ExprBodyRef>,
    env: &EvalEnv,
) -> Result<Value, EvalError> {
    let body = body
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "closure expression has no body"))?;
    let closure_source = state.program.span_for_handle(handle);
    let function = FunctionRef::new(
        state.program.source_id(state.current_source()),
        Arc::from("<closure>"),
        parameters,
        false,
        Some(body),
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
    let body = PreparedBodyRef::from_body_ref(state.program, body)?;
    let bytecode_body = state.program.resolve_bytecode_body(body.body)?;
    let mut env = empty_env();
    let mut locals = empty_bytecode_locals(bytecode_body);
    bind_arguments_into_env(
        state,
        &mut env,
        Some((bytecode_body, &mut locals)),
        &function.parameters,
        args,
        None,
    )?;
    Ok(PreparedEntry { body, env, locals })
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
    let body = PreparedBodyRef::from_body_ref(state.program, body)?;
    let bytecode_body = state.program.resolve_bytecode_body(body.body)?;
    let mut env = empty_env();
    let mut locals = empty_bytecode_locals(bytecode_body);
    for (name, value) in function.captures() {
        env_mut(&mut env).insert(name.to_string(), value.clone());
        set_bytecode_local(bytecode_body, &mut locals, name.as_ref(), value);
    }
    if let Some(self_name) = &function.self_name {
        let self_value = Value::function(function.clone());
        env_mut(&mut env).insert(self_name.to_string(), self_value.clone());
        set_bytecode_local(bytecode_body, &mut locals, self_name.as_ref(), &self_value);
    }
    bind_arguments_into_env(
        state,
        &mut env,
        Some((bytecode_body, &mut locals)),
        &function.parameters,
        args,
        parent_id,
    )?;
    Ok(Some(PreparedEntry { body, env, locals }))
}

pub(super) fn profile_args_fingerprint(args: &[Value]) -> Arc<str> {
    let items = args
        .iter()
        .map(|value| format!("{:?}", value.rep))
        .collect::<Vec<_>>();
    Arc::from(format!("[{}]", items.join(", ")))
}

pub(super) fn authored_call_frame(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    callee: Arc<str>,
    args: &[Value],
    source: SourceSpan,
) -> EvalAuthoredCallFrame {
    EvalAuthoredCallFrame {
        root_node_id: node_id,
        root_work_id: state.current_work,
        callee,
        args_fingerprint: profile_args_fingerprint(args),
        source,
    }
}

pub(super) fn emit_authored_call_started(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    source: SourceSpan,
    frame: &EvalAuthoredCallFrame,
    argc: usize,
) {
    if !state.track_calls {
        return;
    }
    let caller = state.current_call();
    state.emit(
        node_id,
        parent_id,
        Some(source),
        EvalEventKind::AuthoredCallStarted {
            callee: frame.callee.clone(),
            argc: argc as u32,
            args_fingerprint: frame.args_fingerprint.clone(),
            caller_node_id: caller.map(|current| current.root_node_id),
            caller_work_id: caller.and_then(|current| current.root_work_id),
        },
    );
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

pub(super) fn bind_arguments_into_env(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    env: &mut EvalEnv,
    mut bytecode_locals: Option<(&BytecodeBody, &mut [Option<Value>])>,
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
        if let Some((body, locals)) = bytecode_locals.as_mut() {
            set_bytecode_local(body, locals, label.as_str(), &bound);
        }
        env_mut(env).insert(label, bound);
    }
    Ok(())
}

fn eval_node_with_kind<Runtime: RuntimeHooks, Sink: EvalEventSink>(
    state: &mut EvalState<'_, '_, Runtime, Sink>,
    node_id: EvalNodeId,
    kind: EvalExprKind,
    source: SourceSpan,
    run: impl FnOnce(&mut EvalState<'_, '_, Runtime, Sink>) -> Result<EvalStep, EvalError>,
) -> Result<EvalStep, EvalError> {
    let parent_id = state.current_parent();
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

    let result = run(state);

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

fn eval_closure_plan(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    closure: &ClosurePlan,
    env: &EvalEnv,
) -> Result<Value, EvalError> {
    eval_closure_body(
        state,
        closure.handle,
        Arc::clone(&closure.parameters),
        closure.body,
        env,
    )
}

pub(super) fn prepared_body_kind(
    _program: &CompiledProgram,
    body: &PreparedBodyRef,
) -> EvalExprKind {
    body.kind
}

fn eval_prepared_body_step(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    prepared: PreparedEntry,
) -> Result<EvalStep, EvalError> {
    let PreparedEntry { body, env, locals } = prepared;

    fn try_eval_small_prepared_bytecode(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        bytecode_body: &BytecodeBody,
        scope: &EvalEnv,
        locals: &[Option<Value>],
    ) -> Result<Option<EvalStep>, EvalError> {
        match bytecode_body.ops.as_ref() {
            [
                BytecodeOp::EvalReady(ready),
                BytecodeOp::SetLast,
                BytecodeOp::ReturnLast,
            ] => Ok(Some(EvalStep::Ready(eval_ready_plan(
                state, *ready, scope,
            )?))),
            [
                BytecodeOp::LoadLocal { slot, .. },
                BytecodeOp::SetLast,
                BytecodeOp::ReturnLast,
            ] => Ok(Some(EvalStep::Ready(
                locals
                    .get(*slot as usize)
                    .and_then(|value| value.clone())
                    .ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            format!("bytecode local slot {slot} is not initialized"),
                        )
                    })?,
            ))),
            [
                BytecodeOp::EvalStaticMember {
                    field_source,
                    static_member,
                },
                BytecodeOp::SetLast,
                BytecodeOp::ReturnLast,
            ] => Ok(Some(EvalStep::Ready(static_member_value_from_source(
                state,
                field_source.clone(),
                static_member,
            )?))),
            _ => Ok(None),
        }
    }

    let bytecode_body = state.program.resolve_bytecode_body(body.body)?;
    if let Some(result) = try_eval_small_prepared_bytecode(state, bytecode_body, &env, &locals)? {
        return Ok(result);
    }
    continue_bytecode_body(
        state,
        node_id,
        parent_id,
        BytecodeState {
            scope: env,
            locals,
            last: Value::unit(),
            body: body.body,
            pc: 0,
            acc: None,
            temp: None,
            pending: None,
        },
    )
}

pub(super) fn eval_prepared_body_with_node_id(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    prepared: PreparedEntry,
) -> Result<EvalStep, EvalError> {
    let body = prepared.body.clone();
    let root_expr_id = body.root_expr_id();
    let kind = prepared_body_kind(state.program, &body);
    let source = state.program.span_for_expr_id(root_expr_id)?;
    let parent_id = state.current_parent();
    state.with_source(root_expr_id.source(), |state| {
        eval_node_with_kind(state, node_id, kind, source, |state| {
            eval_prepared_body_step(state, node_id, parent_id, prepared)
        })
    })
}

pub(super) fn eval_child_prepared_entry(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    prepared: PreparedEntry,
) -> Result<EvalStep, EvalError> {
    if !state.queue_child_work {
        let child_id = state.alloc_node_id();
        let source = state
            .program
            .span_for_expr_id(prepared.body.root_expr_id())?;
        state.emit_current_dependency(child_id, Some(source));
        return eval_prepared_body_with_node_id(state, child_id, prepared);
    }
    let child_id = queue_child_prepared_entry(state, prepared);
    let resume_node = state
        .current_parent()
        .expect("prepared child entry should have an active parent node");
    Ok(wait_on_child_dependency(
        state,
        resume_node,
        child_id,
        Vec::new(),
    ))
}

pub(super) fn eval_dependent_prepared_entry(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    prepared: PreparedEntry,
) -> Result<EvalStep, EvalError> {
    if state.current_parent() == Some(node_id) {
        eval_child_prepared_entry(state, prepared)
    } else {
        state.with_active_node(node_id, |state| eval_child_prepared_entry(state, prepared))
    }
}

pub(super) fn eval_child_body_ref(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    body: ExprBodyRef,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let prepared = PreparedBodyRef::from_body_ref(state.program, body)?;
    if !state.queue_child_work {
        let child_id = state.alloc_node_id();
        state.emit_current_dependency(
            child_id,
            Some(state.program.span_for_expr_id(prepared.root_expr_id())?),
        );
        return eval_prepared_body_with_node_id(
            state,
            child_id,
            prepared_entry_from_scope(state.program, prepared, env.clone())?,
        );
    }
    let child_id = queue_child_body_ref(state, body, env);
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

pub(super) fn eval_dependent_body_ref(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    body: ExprBodyRef,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    if state.current_parent() == Some(node_id) {
        eval_child_body_ref(state, body, env)
    } else {
        state.with_active_node(node_id, |state| eval_child_body_ref(state, body, env))
    }
}

fn eval_dependent_bytecode_body_ref(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    body: ExprBodyRef,
    bytecode: &BytecodeState,
) -> Result<EvalStep, EvalError> {
    let prepared = prepared_entry_from_bytecode(state.program, body, bytecode)?;
    eval_dependent_prepared_entry(state, node_id, prepared)
}

fn eval_literal_plan(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    literal: &LiteralPlan,
) -> Result<Value, EvalError> {
    let literal_source = match literal {
        LiteralPlan::U64 { handle, .. }
        | LiteralPlan::Bool { handle, .. }
        | LiteralPlan::String { handle, .. }
        | LiteralPlan::RawToken { handle, .. } => state.program.span_for_handle(*handle),
    };
    match literal {
        LiteralPlan::U64 { value, .. } => {
            Ok(Value::u64(*value).with_provenance(Provenance::literal(literal_source)))
        }
        LiteralPlan::Bool { value, .. } => {
            Ok(Value::bool(*value).with_provenance(Provenance::literal(literal_source)))
        }
        LiteralPlan::String { value, .. } => {
            Ok(Value::string(Arc::clone(value))
                .with_provenance(Provenance::literal(literal_source)))
        }
        LiteralPlan::RawToken { kind, text, .. } => match kind {
            SyntaxKind::INT => {
                let value = text.parse::<u64>().map_err(|source| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        format!("invalid integer literal {text}: {source}"),
                    )
                })?;
                Ok(Value::u64(value).with_provenance(Provenance::literal(literal_source)))
            }
            SyntaxKind::TRUE_KW => {
                Ok(Value::bool(true).with_provenance(Provenance::literal(literal_source)))
            }
            SyntaxKind::FALSE_KW => {
                Ok(Value::bool(false).with_provenance(Provenance::literal(literal_source)))
            }
            SyntaxKind::STRING => Ok(Value::string(decode_string_literal(text)?)
                .with_provenance(Provenance::literal(literal_source))),
            other => Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("unsupported literal token: {other:?}"),
            )),
        },
    }
}

fn eval_path_literal_plan(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    path_literal: &PathLiteralPlan,
) -> Result<Value, EvalError> {
    let path_source = state.program.span_for_handle(path_literal.handle);
    if path_literal.interpolated {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "interpolated path literals are not supported yet",
        ));
    }

    let descriptor = path_literal
        .descriptor
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    if descriptor.is_empty() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "path literal had no literal payload",
        ));
    }

    Ok(Value::external("Path", descriptor)
        .with_provenance(Provenance::literal(path_source))
        .with_reuse(ReuseClass::Stable)
        .with_crossing(CrossingPolicy::ByValue))
}

fn eval_ready_plan(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    ready: ExprExecPlan,
    env: &EvalEnv,
) -> Result<Value, EvalError> {
    match ready.ready_kind() {
        ReadyPlanKind::Literal => {
            let literal = state
                .program
                .resolve_literal_plan(ready.literal_plan_id(state.current_source()))?;
            eval_literal_plan(state, literal)
        }
        ReadyPlanKind::PathLiteral => {
            let path_literal = state
                .program
                .resolve_path_literal_plan(ready.path_literal_plan_id(state.current_source()))?;
            eval_path_literal_plan(state, path_literal)
        }
        ReadyPlanKind::Closure => {
            let closure = state
                .program
                .resolve_closure_plan(ready.closure_plan_id(state.current_source()))?;
            eval_closure_plan(state, closure, env)
        }
        ReadyPlanKind::NameRef => {
            let name_ref = state
                .program
                .resolve_name_ref_plan(ready.name_ref_plan_id(state.current_source()))?;
            eval_name_ref_plan(state, name_ref, env)
        }
    }
}

fn ready_plan_source(
    program: &CompiledProgram,
    source: SourceId,
    ready: ExprExecPlan,
) -> Result<SourceSpan, EvalError> {
    match ready.ready_kind() {
        ReadyPlanKind::Literal => {
            let handle = match program.resolve_literal_plan(ready.literal_plan_id(source))? {
                LiteralPlan::U64 { handle, .. }
                | LiteralPlan::Bool { handle, .. }
                | LiteralPlan::String { handle, .. }
                | LiteralPlan::RawToken { handle, .. } => *handle,
            };
            Ok(program.span_for_handle(handle))
        }
        ReadyPlanKind::PathLiteral => Ok(program.span_for_handle(
            program
                .resolve_path_literal_plan(ready.path_literal_plan_id(source))?
                .handle,
        )),
        ReadyPlanKind::Closure => Ok(program.span_for_handle(
            program
                .resolve_closure_plan(ready.closure_plan_id(source))?
                .handle,
        )),
        ReadyPlanKind::NameRef => Ok(program.span_for_handle(
            program
                .resolve_name_ref_plan(ready.name_ref_plan_id(source))?
                .handle,
        )),
    }
}

fn eval_name_ref_plan(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    name_ref: &NameRefPlan,
    env: &EvalEnv,
) -> Result<Value, EvalError> {
    if let Some(resolved) = &name_ref.resolved {
        return resolved_name_ref_value(state, name_ref.handle, resolved);
    }
    let name: Arc<str> = Arc::clone(&name_ref.name);
    if let Some(value) = env.get(name.as_ref()) {
        return Ok(value.clone());
    }
    let reference = name_ref.handle;
    if let Some((resolved_name, function)) =
        state.program.resolve_function_reference(reference, &name)?
    {
        let name_source = state.program.span_for_handle(reference);
        let function = function_ref_from_compiled(
            state,
            resolved_name.clone(),
            function,
            Arc::from([]),
            Some(resolved_name),
        );
        return Ok(Value::function(function)
            .with_provenance(Provenance::binding(name_source, name.to_string())));
    }
    if let Some(constructor) = state
        .program
        .resolve_variant_constructor(name.rsplit('.').next().unwrap_or(&name))
    {
        return Ok(enum_constructor_or_value(
            state,
            constructor.name,
            constructor.fields,
            state.program.span_for_handle(reference),
        ));
    }
    if let Some(newtype_name) = state
        .program
        .resolve_newtype_constructor(name.rsplit('.').next().unwrap_or(&name))
    {
        let source = state.program.span_for_handle(reference);
        let captures = Arc::from([(
            Arc::from("__newtype_ctor"),
            Value::bool(true)
                .with_provenance(Provenance::binding(source.clone(), name.to_string())),
        )]);
        return Ok(Value::function(FunctionRef::new(
            state.program.source_id(state.current_source()),
            newtype_name,
            Arc::from([]),
            false,
            None,
            captures,
            Some(name.clone()),
        ))
        .with_provenance(Provenance::binding(source, name.to_string())));
    }
    Err(EvalError::new(
        EvalErrorKind::Internal,
        format!("name {name} is not bound in this scope"),
    ))
}

fn immediate_call_callee_plan_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    source: SourceId,
    callee: &CallCalleePlan,
    env: &EvalEnv,
) -> Result<Option<Value>, EvalError> {
    match callee.kind() {
        CallCalleePlanKind::NameRef => {
            let name_ref = state
                .program
                .resolve_name_ref_plan(callee.name_ref_plan_id(source))?;
            let name: Arc<str> = Arc::clone(&name_ref.name);
            if let Some(value) = env.get(name.as_ref()) {
                return Ok(Some(value.clone()));
            }
            Ok(Some(Value::function(FunctionRef::new(
                state.program.source_id(source),
                Arc::clone(&name),
                Arc::from([]),
                false,
                None,
                Arc::from([]),
                Some(name),
            ))))
        }
        CallCalleePlanKind::Direct => {
            let direct = state
                .program
                .resolve_direct_call_callee_plan(callee.direct_call_callee_plan_id(source))?;
            immediate_direct_call_callee_plan_value(state, source, direct)
        }
        CallCalleePlanKind::Field => {
            let field = state
                .program
                .resolve_field_plan(callee.field_plan_id(source))?;
            static_namespace_member_plan_value(state, field)
        }
        CallCalleePlanKind::Expr => Ok(None),
    }
}

fn immediate_direct_call_callee_plan_value(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    source: SourceId,
    direct: &DirectCallCalleePlan,
) -> Result<Option<Value>, EvalError> {
    match direct.kind() {
        DirectCallCalleePlanKind::ResolvedFunction => {
            let plan = state
                .program
                .resolve_resolved_function_direct_call_callee_plan(
                    source,
                    direct.resolved_function_index(),
                )?;
            Ok(Some(Value::function(function_ref_from_compiled(
                state,
                plan.resolved_name.clone(),
                plan.function.clone(),
                Arc::from([]),
                Some(plan.resolved_name.clone()),
            ))))
        }
        DirectCallCalleePlanKind::VariantConstructor => {
            let plan = state
                .program
                .resolve_variant_constructor_direct_call_callee_plan(
                    source,
                    direct.variant_constructor_index(),
                )?;
            Ok(Some(enum_constructor_or_value(
                state,
                plan.constructor.name.clone(),
                plan.constructor.fields.clone(),
                state.program.span_for_handle(plan.handle),
            )))
        }
        DirectCallCalleePlanKind::NewtypeConstructor => {
            let plan = state
                .program
                .resolve_newtype_constructor_direct_call_callee_plan(
                    source,
                    direct.newtype_constructor_index(),
                )?;
            let span = state.program.span_for_handle(plan.handle);
            let captures = Arc::from([(
                Arc::from("__newtype_ctor"),
                Value::bool(true).with_provenance(Provenance::binding(
                    span.clone(),
                    plan.display_name.to_string(),
                )),
            )]);
            Ok(Some(
                Value::function(FunctionRef::new(
                    state.program.source_id(source),
                    plan.newtype_name.clone(),
                    Arc::from([]),
                    false,
                    None,
                    captures,
                    Some(plan.display_name.clone()),
                ))
                .with_provenance(Provenance::binding(span, plan.display_name.to_string())),
            ))
        }
        DirectCallCalleePlanKind::StaticMember => {
            let plan = state
                .program
                .resolve_static_member_direct_call_callee_plan(
                    source,
                    direct.static_member_index(),
                )?;
            if let Some(constructor) = &plan.variant_constructor {
                return Ok(Some(enum_constructor_or_value(
                    state,
                    constructor.name.clone(),
                    constructor.fields.clone(),
                    state.program.span_for_handle(plan.handle),
                )));
            }
            let source = state.program.span_for_handle(plan.handle);
            Ok(Some(
                Value::function(FunctionRef::new(
                    state.program.source_id(state.current_source()),
                    plan.full_name.clone(),
                    Arc::from([]),
                    false,
                    None,
                    Arc::from([]),
                    None,
                ))
                .with_provenance(Provenance::binding(source, plan.full_name.to_string())),
            ))
        }
    }
}

fn eval_bytecode_call_callee_plan(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    source: SourceId,
    callee: &CallCalleePlan,
    bytecode: &mut BytecodeState,
) -> Result<EvalStep, EvalError> {
    if let Some(value) = immediate_call_callee_plan_value(state, source, callee, &bytecode.scope)? {
        return Ok(EvalStep::Ready(value));
    }
    match callee.kind() {
        CallCalleePlanKind::Expr => {
            eval_dependent_bytecode_body_ref(state, node_id, callee.expr_body(), bytecode)
        }
        CallCalleePlanKind::Field => {
            let field = state
                .program
                .resolve_field_plan(callee.field_plan_id(source))?;
            eval_field_plan(state, field, &mut bytecode.scope)
        }
        CallCalleePlanKind::NameRef | CallCalleePlanKind::Direct => Err(EvalError::new(
            EvalErrorKind::Internal,
            "direct call callee should resolve immediately",
        )),
    }
}

fn static_namespace_member_plan_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    field: &FieldPlan,
) -> Result<Option<Value>, EvalError> {
    if let Some(static_member) = &field.static_member {
        return static_member_value(state, field.handle, static_member).map(Some);
    }
    Ok(None)
}

fn resolved_name_ref_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    handle: ProgramNodeHandle,
    resolved: &ResolvedNameRef,
) -> Result<Value, EvalError> {
    match resolved {
        ResolvedNameRef::Function {
            resolved_name,
            function,
        } => Ok(Value::function(function_ref_from_compiled(
            state,
            resolved_name.clone(),
            function.clone(),
            Arc::from([]),
            Some(resolved_name.clone()),
        ))
        .with_provenance(Provenance::binding(
            state.program.span_for_handle(handle),
            resolved_name.to_string(),
        ))),
        ResolvedNameRef::VariantConstructor(constructor) => Ok(enum_constructor_or_value(
            state,
            constructor.name.clone(),
            constructor.fields.clone(),
            state.program.span_for_handle(handle),
        )),
        ResolvedNameRef::NewtypeConstructor {
            display_name,
            newtype_name,
        } => {
            let source = state.program.span_for_handle(handle);
            let captures = Arc::from([(
                Arc::from("__newtype_ctor"),
                Value::bool(true).with_provenance(Provenance::binding(
                    source.clone(),
                    display_name.to_string(),
                )),
            )]);
            Ok(Value::function(FunctionRef::new(
                state.program.source_id(state.current_source()),
                newtype_name.clone(),
                Arc::from([]),
                false,
                None,
                captures,
                Some(display_name.clone()),
            ))
            .with_provenance(Provenance::binding(source, display_name.to_string())))
        }
    }
}

fn static_member_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    handle: ProgramNodeHandle,
    static_member: &StaticMemberPlan,
) -> Result<Value, EvalError> {
    static_member_value_from_source(state, state.program.span_for_handle(handle), static_member)
}

fn static_member_value_from_source(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    source: SourceSpan,
    static_member: &StaticMemberPlan,
) -> Result<Value, EvalError> {
    if let Some(constructor) = &static_member.variant_constructor {
        return Ok(enum_constructor_or_value(
            state,
            constructor.name.clone(),
            constructor.fields.clone(),
            source,
        ));
    }
    Ok(Value::function(FunctionRef::new(
        state.program.source_id(state.current_source()),
        static_member.full_name.clone(),
        Arc::from([]),
        false,
        None,
        Arc::from([]),
        None,
    ))
    .with_provenance(Provenance::binding(
        source,
        static_member.full_name.to_string(),
    )))
}

fn bound_method_name(receiver: &Value, field_name: &str) -> Option<&'static str> {
    match (&receiver.rep, field_name) {
        (ValueRep::External(external), "utf8") if external.kind.as_ref() == "Blob" => {
            Some("Blob.utf8")
        }
        (ValueRep::External(external), "blob") if external.kind.as_ref() == "Bytes" => {
            Some("Bytes.blob")
        }
        (ValueRep::External(external), "utf8") if external.kind.as_ref() == "Bytes" => {
            Some("Bytes.utf8")
        }
        (ValueRep::External(external), "utf8_lossy") if external.kind.as_ref() == "Bytes" => {
            Some("Bytes.utf8_lossy")
        }
        (ValueRep::External(external), "to_string") if external.kind.as_ref() == "Path" => {
            Some("Path.to_string")
        }
        (ValueRep::External(external), "file") if external.kind.as_ref() == "Tree" => {
            Some("Tree.file")
        }
        (ValueRep::External(external), "subtree") if external.kind.as_ref() == "Tree" => {
            Some("Tree.subtree")
        }
        (ValueRep::External(external), "merge") if external.kind.as_ref() == "Tree" => {
            Some("Tree.merge")
        }
        (ValueRep::External(external), "collect")
            if external.kind.as_ref().starts_with("Stream[") =>
        {
            Some("Stream.collect")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "merge")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.merge")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "for_each")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.for_each")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "filter")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.filter")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "flatten")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.flatten")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "flat_map")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.flat_map")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "map")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.map")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "to_array")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.to_array")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "to_map")
            if type_name.as_ref() == "__Eval2Stream" =>
        {
            Some("Stream.to_map")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "file")
            if type_name.as_ref() == "__Eval2MergedTree" =>
        {
            Some("Tree.file")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "subtree")
            if type_name.as_ref() == "__Eval2MergedTree" =>
        {
            Some("Tree.subtree")
        }
        (ValueRep::Struct(StructValue { type_name, .. }), "merge")
            if type_name.as_ref() == "__Eval2MergedTree" =>
        {
            Some("Tree.merge")
        }
        (ValueRep::External(external), "os") if external.kind.as_ref() == "Host" => Some("Host.os"),
        (ValueRep::External(external), "arch") if external.kind.as_ref() == "Host" => {
            Some("Host.arch")
        }
        (ValueRep::External(external), "which") if external.kind.as_ref() == "Host" => {
            Some("Host.which")
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
        (ValueRep::Array(_), "find") => Some("Array.find"),
        (ValueRep::Array(_), "flat_map") => Some("Array.flat_map"),
        (ValueRep::Array(_), "filter_map") => Some("Array.filter_map"),
        (ValueRep::Array(_), "flatten") => Some("Array.flatten"),
        (ValueRep::Array(_), "filter") => Some("Array.filter"),
        (ValueRep::Array(_), "merge") => Some("Array.merge"),
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
    method_name: &'static str,
    field_source: SourceSpan,
) -> Value {
    let function = FunctionRef::new(
        state.program.source_id(state.current_source()),
        Arc::from(method_name),
        Arc::from([]),
        false,
        None,
        Arc::from(vec![(Arc::from("self"), receiver)]),
        None,
    );
    Value::function(function)
        .with_provenance(Provenance::binding(field_source, method_name.to_string()))
}

pub(super) fn project_field_or_method_value(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    receiver: Value,
    field_name: Arc<str>,
    field_source: SourceSpan,
) -> Result<Value, EvalError> {
    if let Some(method_name) = bound_method_name(&receiver, field_name.as_ref()) {
        return Ok(bound_method_value(
            state,
            receiver,
            method_name,
            field_source,
        ));
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

fn newtype_constructor_name(function: &FunctionRef) -> Option<Arc<str>> {
    let marker = function
        .captures
        .iter()
        .find(|(name, _)| name.as_ref() == "__newtype_ctor")?;
    (marker.1.rep == ValueRep::Bool(true)).then(|| function.name.clone())
}

fn dispatch_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    call_source: SourceSpan,
    type_args: Arc<[Ty]>,
    callee: Value,
    mut args: Vec<Value>,
) -> Result<EvalStep, EvalError> {
    let ValueRep::Function(function) = &callee.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "call target must evaluate to a function value",
        ));
    };
    if let Some(target) = fallible_target_function(function) {
        let fallible_source = call_source.clone();
        return match dispatch_call(
            state,
            node_id,
            parent_id,
            call_source,
            type_args,
            Value::function(target),
            args,
        ) {
            Ok(EvalStep::Ready(value)) => {
                Ok(EvalStep::Ready(fallible_some(value, fallible_source)))
            }
            Ok(EvalStep::Suspended(suspension)) => Ok(extend_suspension(
                suspension,
                ContinuationFrame::Fallible {
                    node_id,
                    parent_id,
                    fallible_source,
                },
            )),
            Err(error) => Ok(wrap_fallible_result(Err(error), fallible_source)),
        };
    }
    if let Some(prepared) = prepare_function_call(state, function, &args, Some(node_id))? {
        return if !state.track_calls {
            eval_dependent_prepared_entry(state, node_id, prepared)
        } else {
            let frame = authored_call_frame(
                state,
                node_id,
                function.name.clone(),
                &args,
                call_source.clone(),
            );
            emit_authored_call_started(
                state,
                node_id,
                parent_id,
                call_source.clone(),
                &frame,
                args.len(),
            );
            state.with_authored_call(frame, |state| {
                eval_dependent_prepared_entry(state, node_id, prepared)
            })
        };
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
    if let Some(newtype_name) = newtype_constructor_name(function) {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                format!(
                    "newtype constructor {} expected 1 argument, got {}",
                    function.name,
                    args.len()
                ),
            ));
        }
        let value = Value::structure(newtype_name, [(Arc::from("value"), args.remove(0))])
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
    if let Some(step) = continue_builtin_call(
        state,
        node_id,
        parent_id,
        function,
        &callee_name,
        call_source.clone(),
        &type_args,
        &args,
    )? {
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
        .call(state.place, function, &args, &call_source)?
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
                state,
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

fn struct_literal_type_name(type_name: Option<Arc<str>>) -> Result<Arc<str>, EvalError> {
    type_name
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "struct literal has no type name"))
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

fn fallible_some(value: Value, source: SourceSpan) -> Value {
    let fields = vec![(Arc::from("value"), value)];
    let meta = aggregate_child_meta(
        Provenance::literal(source),
        fields.iter().map(|(_, value)| value),
    );
    Value::structure("Some", fields).with_meta(meta)
}

fn fallible_none(source: SourceSpan) -> Value {
    Value::structure("None", Vec::<(Arc<str>, Value)>::new())
        .with_provenance(Provenance::literal(source))
}

fn fallible_function_value(function: &FunctionRef, source: SourceSpan) -> Value {
    let mut captures = function.captures().iter().cloned().collect::<Vec<_>>();
    captures.push((
        Arc::from("__fallible_target"),
        Value::function(function.clone()).with_provenance(Provenance::literal(source.clone())),
    ));
    let wrapper = FunctionRef::new(
        function.source_id.clone(),
        function.name.clone(),
        function.parameters.clone(),
        function.has_keyword_parameters,
        None,
        Arc::from(captures),
        function.self_name.clone(),
    );
    Value::function(wrapper).with_provenance(Provenance::literal(source))
}

fn wrap_fallible_result(result: Result<Value, EvalError>, source: SourceSpan) -> EvalStep {
    let value = match result {
        Ok(value) => match &value.rep {
            ValueRep::Function(function) => fallible_function_value(function, source),
            _ => fallible_some(value, source),
        },
        Err(_) => fallible_none(source),
    };
    EvalStep::Ready(value)
}

fn fallible_target_function(function: &FunctionRef) -> Option<FunctionRef> {
    function
        .captures()
        .iter()
        .find(|(name, _)| name.as_ref() == "__fallible_target")
        .and_then(|(_, value)| match &value.rep {
            ValueRep::Function(target) => Some(target.clone()),
            _ => None,
        })
}

fn continue_bytecode_fallible(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    bytecode: &mut BytecodeState,
    fallible_source: SourceSpan,
    expr: Option<ExprBodyRef>,
    resumed: Option<Result<Value, EvalError>>,
) -> Result<EvalStep, EvalError> {
    if let Some(result) = resumed {
        return Ok(wrap_fallible_result(result, fallible_source));
    }
    let Some(expr) = expr else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "fallible expression has no operand",
        ));
    };
    match eval_dependent_body_ref(state, node_id, expr, &mut bytecode.scope) {
        Ok(EvalStep::Ready(value)) => Ok(wrap_fallible_result(Ok(value), fallible_source)),
        Ok(EvalStep::Suspended(suspension)) => {
            bytecode.pending = Some(BytecodePendingState::Fallible { fallible_source });
            Ok(EvalStep::Suspended(suspension))
        }
        Err(error) => Ok(wrap_fallible_result(Err(error), fallible_source)),
    }
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

fn eval_field_plan(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    field: &FieldPlan,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    if let Some(value) = static_namespace_member_plan_value(state, field)? {
        return Ok(EvalStep::Ready(value));
    }
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("field expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let receiver = field.receiver.ok_or_else(|| {
        EvalError::new(EvalErrorKind::Internal, "field expression has no receiver")
    })?;
    let field_source = state.program.span_for_handle(field.handle);
    let field_state = FieldState {
        field_expr: state.expr_id_for(field.handle)?,
    };
    let receiver = match eval_child_body_ref(state, receiver, env)? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::Field {
                    node_id,
                    parent_id,
                    field: field_state,
                },
            ));
        }
    };
    let field_name = field.field_name.as_ref().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            "field expression has no field name",
        )
    })?;
    project_field_or_method_value(state, receiver, field_name.clone(), field_source)
        .map(EvalStep::Ready)
}

pub(super) fn project_field_expr_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    receiver: Value,
    field_expr: ExprId,
) -> Result<Value, EvalError> {
    let field = state.program.resolve_expr_id(field_expr)?;
    let ExprKind::Field(field) = field.kind() else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "field continuation targeted non-field expression",
        ));
    };
    let field_name = field.field_name.as_ref().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            "field expression has no field name",
        )
    })?;
    let field_source = state.program.span_for_handle(field.handle);
    project_field_or_method_value(state, receiver, field_name.clone(), field_source)
}

pub(super) fn continue_bytecode_body(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut bytecode: BytecodeState,
) -> Result<EvalStep, EvalError> {
    fn eval_inline_ready_bytecode_plan(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        source: SourceId,
        ready: ExprExecPlan,
        env: &EvalEnv,
    ) -> Result<Value, EvalError> {
        let ready_source = ready_plan_source(state.program, source, ready)?;
        state.emit(
            node_id,
            parent_id,
            Some(ready_source.clone()),
            EvalEventKind::EnterExpr {
                kind: ready.trace_kind(),
            },
        );
        match eval_ready_plan(state, ready, env) {
            Ok(value) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(ready_source),
                    EvalEventKind::ExitReady {
                        reuse: value.meta.reuse.clone(),
                        crossing: value.meta.crossing.clone(),
                    },
                );
                Ok(value)
            }
            Err(error) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(ready_source),
                    EvalEventKind::ExitPoison {
                        kind: error.kind,
                        message: error.message.clone(),
                    },
                );
                Err(error)
            }
        }
    }

    fn suspend_bytecode(
        suspension: EvalSuspension,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: BytecodeState,
    ) -> EvalStep {
        extend_suspension(
            suspension,
            ContinuationFrame::Bytecode {
                node_id,
                parent_id,
                bytecode,
            },
        )
    }

    fn continue_bytecode_match_arms(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        _parent_id: Option<EvalNodeId>,
        match_expr: &MatchPlan,
        scrutinee: Value,
        scope: &EvalEnv,
    ) -> Result<EvalStep, EvalError> {
        for arm in match_expr.arms.iter() {
            let mut arm_env = scope.clone();
            if match_pattern(state, &mut arm_env, &arm.pattern, scrutinee.clone())? {
                return eval_dependent_body_ref(state, node_id, arm.body, &mut arm_env);
            }
        }
        Err(EvalError::new(
            EvalErrorKind::Internal,
            "match expression had no matching arm",
        ))
    }

    fn continue_bytecode_match(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        match_state: BytecodeMatchState,
        resumed_scrutinee: Option<Value>,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_match(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    match_state,
                    resumed_scrutinee,
                )
            });
        }
        let scrutinee = if let Some(scrutinee) = resumed_scrutinee {
            scrutinee
        } else {
            let scrutinee_expr = match_state.match_plan.scrutinee.ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "match expression has no scrutinee")
            })?;
            match eval_dependent_bytecode_body_ref(state, node_id, scrutinee_expr, bytecode)? {
                EvalStep::Ready(value) => value,
                EvalStep::Suspended(suspension) => {
                    bytecode.pending = Some(BytecodePendingState::Match { match_state });
                    return Ok(EvalStep::Suspended(suspension));
                }
            }
        };
        continue_bytecode_match_arms(
            state,
            node_id,
            parent_id,
            &match_state.match_plan,
            scrutinee,
            &bytecode.scope,
        )
    }

    fn init_bytecode_where_state(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        where_expr: &WherePlan,
        env: &EvalEnv,
    ) -> Result<BytecodeWhereState, EvalError> {
        let call_expr = where_expr.call.as_ref().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "where expression currently requires a compiled call plan",
            )
        })?;
        let callee = call_expr.callee.as_ref().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "call expression has no callee")
        })?;
        let Some(callee_value) =
            immediate_call_callee_plan_value(state, call_expr.handle.source(), callee, env)?
        else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "where call callee must resolve immediately for now",
            ));
        };
        let mut keyword_bindings = Vec::new();
        let mut remaining_keyword_bindings = Vec::new();
        for binding in where_expr.keyword_bindings.iter() {
            let name = binding.name.clone();
            match binding.expr {
                Some(expr) => remaining_keyword_bindings.push((name, expr)),
                None => {
                    let Some(value) = env.get(name.as_ref()) else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            format!("where binding {name} is not available in scope"),
                        ));
                    };
                    keyword_bindings.push((name, value.clone()));
                }
            }
        }
        Ok(BytecodeWhereState {
            call: WhereCallPlan {
                call_source: state.program.span_for_handle(call_expr.handle),
                callee: callee_value,
                type_args: Arc::clone(&call_expr.type_args),
                positional_args: Arc::clone(&call_expr.positional_args),
            },
            pending_keyword_name: None,
            keyword_bindings,
            remaining_keyword_bindings,
        })
    }

    fn continue_bytecode_planned_call(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut call: BytecodePlannedCallState,
        resumed: Option<PlannedCallResumeKind>,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_planned_call(state, node_id, parent_id, bytecode, call, resumed)
            });
        }
        if let Some(resumed) = resumed {
            match resumed {
                PlannedCallResumeKind::Callee => {
                    call.callee = Some(bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode call resumed without a callee value",
                        )
                    })?);
                }
                PlannedCallResumeKind::Arg => {
                    call.args.push(bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode call resumed without an argument value",
                        )
                    })?);
                }
                PlannedCallResumeKind::JoinArgs => {
                    let _ = bytecode.acc.take();
                    let queued_arg_nodes = call.queued_arg_nodes.clone();
                    let mut args = Vec::with_capacity(queued_arg_nodes.len());
                    for child_node in &queued_arg_nodes {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => args.push(value.clone()),
                            Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                            _ => {
                                bytecode.pending = Some(BytecodePendingState::Call {
                                    call,
                                    pending: PlannedCallResumeKind::JoinArgs,
                                });
                                return Ok(wait_on_child_dependency(
                                    state,
                                    node_id,
                                    *child_node,
                                    Vec::new(),
                                ));
                            }
                        }
                    }
                    call.args = args;
                    call.next_arg_index = call.positional_args.len();
                    call.queued_arg_nodes.clear();
                }
            }
        }

        if call.callee.is_none() {
            let callee = call.callee_plan.as_ref().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "call expression has no callee")
            })?;
            if let Some(callee_value) =
                immediate_call_callee_plan_value(state, call.plan_source, callee, &bytecode.scope)?
            {
                if state.queue_child_work
                    && !call.positional_args.is_empty()
                    && call.args.is_empty()
                {
                    call.callee = Some(callee_value);
                    call.next_arg_index = call.positional_args.len();
                    let queued_arg_nodes = call
                        .positional_args
                        .iter()
                        .map(|arg| {
                            queue_child_prepared_entry(
                                state,
                                prepared_entry_from_bytecode(state.program, *arg, bytecode)
                                    .expect("bytecode call arg should resolve to a prepared entry"),
                            )
                        })
                        .collect::<Vec<_>>();
                    let first_child = queued_arg_nodes[0];
                    call.queued_arg_nodes = queued_arg_nodes;
                    bytecode.pending = Some(BytecodePendingState::Call {
                        call,
                        pending: PlannedCallResumeKind::JoinArgs,
                    });
                    return Ok(wait_on_child_dependency(
                        state,
                        node_id,
                        first_child,
                        Vec::new(),
                    ));
                }
                call.callee = Some(callee_value);
            } else {
                match eval_bytecode_call_callee_plan(
                    state,
                    node_id,
                    call.plan_source,
                    callee,
                    bytecode,
                )? {
                    EvalStep::Ready(value) => call.callee = Some(value),
                    EvalStep::Suspended(suspension) => {
                        bytecode.pending = Some(BytecodePendingState::Call {
                            call,
                            pending: PlannedCallResumeKind::Callee,
                        });
                        return Ok(EvalStep::Suspended(suspension));
                    }
                }
            }
        }

        while let Some(arg_handle) = call.positional_args.get(call.next_arg_index).copied() {
            call.next_arg_index += 1;
            let arg_index = call.args.len();
            let callee = call
                .callee
                .as_ref()
                .expect("planned call should have a callee before evaluating arguments");
            let adapter = function_adapter_value_for_arg(
                state,
                callee,
                arg_index,
                arg_handle,
                &bytecode.scope,
            )?;
            let arg_step = match adapter {
                Some(value) => EvalStep::Ready(value),
                None => eval_dependent_bytecode_body_ref(state, node_id, arg_handle, bytecode)?,
            };
            match arg_step {
                EvalStep::Ready(value) => call.args.push(value),
                EvalStep::Suspended(suspension) => {
                    bytecode.pending = Some(BytecodePendingState::Call {
                        call,
                        pending: PlannedCallResumeKind::Arg,
                    });
                    return Ok(EvalStep::Suspended(suspension));
                }
            }
        }

        debug_assert!(call.queued_arg_nodes.is_empty());
        dispatch_call(
            state,
            node_id,
            parent_id,
            call.call_source,
            Arc::clone(&call.type_args),
            call.callee
                .expect("planned call should have a callee before dispatch"),
            call.args,
        )
    }

    fn continue_bytecode_where(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut where_state: BytecodeWhereState,
        resumed_keyword_value: Option<Value>,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_where(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    where_state,
                    resumed_keyword_value,
                )
            });
        }
        if let Some(value) = resumed_keyword_value {
            let pending_keyword_name =
                where_state.pending_keyword_name.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "where continuation is missing its pending keyword binding name",
                    )
                })?;
            where_state
                .keyword_bindings
                .push((pending_keyword_name, value));
        }

        let mut remaining = where_state.remaining_keyword_bindings.into_iter();
        while let Some((name, expr)) = remaining.next() {
            match eval_dependent_bytecode_body_ref(state, node_id, expr, bytecode)? {
                EvalStep::Ready(value) => where_state.keyword_bindings.push((name, value)),
                EvalStep::Suspended(suspension) => {
                    where_state.pending_keyword_name = Some(name);
                    where_state.remaining_keyword_bindings = remaining.collect();
                    bytecode.pending = Some(BytecodePendingState::Where { where_state });
                    return Ok(EvalStep::Suspended(suspension));
                }
            }
        }

        let WhereCallPlan {
            call_source,
            callee,
            type_args,
            positional_args,
        } = where_state.call;
        continue_bytecode_planned_call(
            state,
            node_id,
            parent_id,
            bytecode,
            BytecodePlannedCallState {
                plan_source: state.current_source,
                call_source,
                callee_plan: None,
                type_args,
                positional_args,
                callee: Some(apply_where_keyword_bindings(
                    callee,
                    where_state.keyword_bindings,
                )),
                args: Vec::new(),
                next_arg_index: 0,
                queued_arg_nodes: Vec::new(),
            },
            None,
        )
    }

    fn start_bytecode_array(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        array: &ArrayPlan,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                start_bytecode_array(state, node_id, parent_id, bytecode, array)
            });
        }
        if state.queue_child_work {
            let mut child_nodes = Vec::new();
            for item in array.items.iter() {
                match item {
                    ArrayPlanItem::Expr(body) => {
                        child_nodes.push(ArrayJoinItem::Expr(queue_child_prepared_entry(
                            state,
                            prepared_entry_from_bytecode(state.program, *body, bytecode)?,
                        )))
                    }
                    ArrayPlanItem::Spread(body) => {
                        child_nodes.push(ArrayJoinItem::Spread(queue_child_prepared_entry(
                            state,
                            prepared_entry_from_bytecode(state.program, *body, bytecode)?,
                        )))
                    }
                }
            }
            if child_nodes.is_empty() {
                let provenance = Provenance::literal(state.program.span_for_handle(array.handle));
                return Ok(EvalStep::Ready(Value::array(Vec::new()).with_meta(
                    ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue),
                )));
            }
            let first_child = match child_nodes[0] {
                ArrayJoinItem::Expr(child_id) | ArrayJoinItem::Spread(child_id) => child_id,
            };
            bytecode.pending = Some(BytecodePendingState::ArrayJoin {
                child_nodes: child_nodes.clone(),
                array_source: state.program.span_for_handle(array.handle),
            });
            return Ok(wait_on_child_dependency(
                state,
                node_id,
                first_child,
                Vec::new(),
            ));
        }
        continue_bytecode_array_items(
            state,
            node_id,
            parent_id,
            bytecode,
            BytecodeArrayState {
                items: Vec::new(),
                spread: false,
                remaining_items: array
                    .items
                    .iter()
                    .cloned()
                    .map(|item| match item {
                        ArrayPlanItem::Expr(body) => ArrayEvalItem::Expr(body),
                        ArrayPlanItem::Spread(body) => ArrayEvalItem::Spread(body),
                    })
                    .collect(),
                array_source: state.program.span_for_handle(array.handle),
            },
        )
    }

    fn continue_bytecode_array(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut array: BytecodeArrayState,
        resumed_value: Value,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_array(state, node_id, parent_id, bytecode, array, resumed_value)
            });
        }
        append_array_item(&mut array.items, resumed_value, array.spread)?;
        continue_bytecode_array_items(state, node_id, parent_id, bytecode, array)
    }

    fn continue_bytecode_array_items(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        _parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut array: BytecodeArrayState,
    ) -> Result<EvalStep, EvalError> {
        let mut remaining = array.remaining_items.into_iter();
        while let Some(item_handle) = remaining.next() {
            let (item_handle, spread) = match item_handle {
                ArrayEvalItem::Expr(handle) => (handle, false),
                ArrayEvalItem::Spread(handle) => (handle, true),
            };
            match eval_dependent_bytecode_body_ref(state, node_id, item_handle, bytecode)? {
                EvalStep::Ready(value) => append_array_item(&mut array.items, value, spread)?,
                EvalStep::Suspended(suspension) => {
                    array.spread = spread;
                    array.remaining_items = remaining.collect();
                    bytecode.pending = Some(BytecodePendingState::Array { array });
                    return Ok(EvalStep::Suspended(suspension));
                }
            }
        }
        let provenance = Provenance::literal(array.array_source);
        let meta = aggregate_child_meta(provenance, array.items.iter());
        Ok(EvalStep::Ready(Value::array(array.items).with_meta(meta)))
    }

    fn continue_bytecode_array_join(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        bytecode: &mut BytecodeState,
        child_nodes: Vec<ArrayJoinItem>,
        array_source: SourceSpan,
    ) -> Result<EvalStep, EvalError> {
        let _ = bytecode.acc.take();
        let mut items = Vec::new();
        for child_node in &child_nodes {
            let (child_node_id, spread) = match child_node {
                ArrayJoinItem::Expr(child_node_id) => (*child_node_id, false),
                ArrayJoinItem::Spread(child_node_id) => (*child_node_id, true),
            };
            match state.scheduler.node_state(child_node_id) {
                Some(EvalNodeState::Ready(value)) => {
                    append_array_item(&mut items, value.clone(), spread)?
                }
                Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                _ => {
                    bytecode.pending = Some(BytecodePendingState::ArrayJoin {
                        child_nodes: child_nodes.clone(),
                        array_source: array_source.clone(),
                    });
                    return Ok(wait_on_child_dependency(
                        state,
                        node_id,
                        child_node_id,
                        Vec::new(),
                    ));
                }
            }
        }
        let provenance = Provenance::literal(array_source);
        let meta = aggregate_child_meta(provenance, items.iter());
        Ok(EvalStep::Ready(Value::array(items).with_meta(meta)))
    }

    fn start_bytecode_tuple(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        tuple: &TuplePlan,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                start_bytecode_tuple(state, node_id, parent_id, bytecode, tuple)
            });
        }
        if state.queue_child_work {
            let child_nodes = tuple
                .items
                .iter()
                .map(|item| {
                    prepared_entry_from_bytecode(state.program, *item, bytecode)
                        .map(|prepared| queue_child_prepared_entry(state, prepared))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if child_nodes.is_empty() {
                let provenance = Provenance::literal(state.program.span_for_handle(tuple.handle));
                return Ok(EvalStep::Ready(Value::tuple(Vec::new()).with_meta(
                    ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue),
                )));
            }
            bytecode.pending = Some(BytecodePendingState::TupleJoin {
                child_nodes: child_nodes.clone(),
                tuple_source: state.program.span_for_handle(tuple.handle),
            });
            return Ok(wait_on_child_dependency(
                state,
                node_id,
                child_nodes[0],
                Vec::new(),
            ));
        }
        continue_bytecode_tuple_items(
            state,
            node_id,
            parent_id,
            bytecode,
            BytecodeTupleState {
                items: Vec::new(),
                remaining_items: tuple.items.to_vec(),
                tuple_source: state.program.span_for_handle(tuple.handle),
            },
        )
    }

    fn continue_bytecode_tuple(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut tuple: BytecodeTupleState,
        resumed_value: Value,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_tuple(state, node_id, parent_id, bytecode, tuple, resumed_value)
            });
        }
        tuple.items.push(resumed_value);
        continue_bytecode_tuple_items(state, node_id, parent_id, bytecode, tuple)
    }

    fn continue_bytecode_tuple_items(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        _parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut tuple: BytecodeTupleState,
    ) -> Result<EvalStep, EvalError> {
        let mut remaining = tuple.remaining_items.into_iter();
        while let Some(item_handle) = remaining.next() {
            match eval_dependent_bytecode_body_ref(state, node_id, item_handle, bytecode)? {
                EvalStep::Ready(value) => tuple.items.push(value),
                EvalStep::Suspended(suspension) => {
                    tuple.remaining_items = remaining.collect();
                    bytecode.pending = Some(BytecodePendingState::Tuple { tuple });
                    return Ok(EvalStep::Suspended(suspension));
                }
            }
        }
        let provenance = Provenance::literal(tuple.tuple_source);
        let meta = aggregate_child_meta(provenance, tuple.items.iter());
        Ok(EvalStep::Ready(Value::tuple(tuple.items).with_meta(meta)))
    }

    fn continue_bytecode_tuple_join(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        bytecode: &mut BytecodeState,
        child_nodes: Vec<EvalNodeId>,
        tuple_source: SourceSpan,
    ) -> Result<EvalStep, EvalError> {
        let _ = bytecode.acc.take();
        let mut items = Vec::with_capacity(child_nodes.len());
        for child_node in &child_nodes {
            match state.scheduler.node_state(*child_node) {
                Some(EvalNodeState::Ready(value)) => items.push(value.clone()),
                Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                _ => {
                    bytecode.pending = Some(BytecodePendingState::TupleJoin {
                        child_nodes: child_nodes.clone(),
                        tuple_source: tuple_source.clone(),
                    });
                    return Ok(wait_on_child_dependency(
                        state,
                        node_id,
                        *child_node,
                        Vec::new(),
                    ));
                }
            }
        }
        let provenance = Provenance::literal(tuple_source);
        let meta = aggregate_child_meta(provenance, items.iter());
        Ok(EvalStep::Ready(Value::tuple(items).with_meta(meta)))
    }

    fn start_bytecode_range(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        range: &RangePlan,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                start_bytecode_range(state, node_id, parent_id, bytecode, range)
            });
        }
        if state.queue_child_work {
            let start_child = range
                .start
                .map(|body| prepared_entry_from_bytecode(state.program, body, bytecode))
                .transpose()?
                .map(|prepared| queue_child_prepared_entry(state, prepared));
            let end_child = range
                .end
                .map(|body| prepared_entry_from_bytecode(state.program, body, bytecode))
                .transpose()?
                .map(|prepared| queue_child_prepared_entry(state, prepared));
            if let Some(child_id) = start_child.or(end_child) {
                bytecode.pending = Some(BytecodePendingState::RangeJoin {
                    start_child,
                    end_child,
                    range_source: state.program.span_for_handle(range.handle),
                    inclusive_end: range.inclusive_end,
                });
                return Ok(wait_on_child_dependency(
                    state,
                    node_id,
                    child_id,
                    Vec::new(),
                ));
            }
            return Ok(EvalStep::Ready(ready_range_value(
                None,
                None,
                range.inclusive_end,
                state.program.span_for_handle(range.handle),
            )));
        }
        let range_source = state.program.span_for_handle(range.handle);
        match range.start {
            Some(start) => match eval_dependent_bytecode_body_ref(state, node_id, start, bytecode)?
            {
                EvalStep::Ready(start) => continue_bytecode_range_end(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    Some(start),
                    range.end,
                    range_source,
                    range.inclusive_end,
                ),
                EvalStep::Suspended(suspension) => {
                    bytecode.pending = Some(BytecodePendingState::Range {
                        range: BytecodeRangeState {
                            start: None,
                            end: range.end,
                            range_source,
                            inclusive_end: range.inclusive_end,
                        },
                        pending: RangeResumeKind::Start,
                    });
                    Ok(EvalStep::Suspended(suspension))
                }
            },
            None => continue_bytecode_range_end(
                state,
                node_id,
                parent_id,
                bytecode,
                None,
                range.end,
                range_source,
                range.inclusive_end,
            ),
        }
    }

    fn continue_bytecode_range(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        range: BytecodeRangeState,
        pending: RangeResumeKind,
        resumed_value: Value,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_range(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    range,
                    pending,
                    resumed_value,
                )
            });
        }
        let step = match pending {
            RangeResumeKind::Start => continue_bytecode_range_end(
                state,
                node_id,
                parent_id,
                bytecode,
                Some(resumed_value),
                range.end,
                range.range_source,
                range.inclusive_end,
            )?,
            RangeResumeKind::End => EvalStep::Ready(build_range_value(
                range.start,
                Some(resumed_value),
                range.inclusive_end,
                range.range_source,
            )?),
        };
        Ok(step)
    }

    fn continue_bytecode_range_end(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        _parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        start: Option<Value>,
        end: Option<ExprBodyRef>,
        range_source: SourceSpan,
        inclusive_end: bool,
    ) -> Result<EvalStep, EvalError> {
        let end = match end {
            Some(end_handle) => {
                match eval_dependent_bytecode_body_ref(state, node_id, end_handle, bytecode)? {
                    EvalStep::Ready(end) => Some(end),
                    EvalStep::Suspended(suspension) => {
                        bytecode.pending = Some(BytecodePendingState::Range {
                            range: BytecodeRangeState {
                                start,
                                end: None,
                                range_source,
                                inclusive_end,
                            },
                            pending: RangeResumeKind::End,
                        });
                        return Ok(EvalStep::Suspended(suspension));
                    }
                }
            }
            None => None,
        };
        Ok(EvalStep::Ready(build_range_value(
            start,
            end,
            inclusive_end,
            range_source,
        )?))
    }

    fn continue_bytecode_range_join(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        bytecode: &mut BytecodeState,
        start_child: Option<EvalNodeId>,
        end_child: Option<EvalNodeId>,
        range_source: SourceSpan,
        inclusive_end: bool,
    ) -> Result<EvalStep, EvalError> {
        let _ = bytecode.acc.take();
        let start = match start_child {
            Some(child_node) => match state.scheduler.node_state(child_node) {
                Some(EvalNodeState::Ready(value)) => Some(value.clone()),
                Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                _ => {
                    bytecode.pending = Some(BytecodePendingState::RangeJoin {
                        start_child,
                        end_child,
                        range_source: range_source.clone(),
                        inclusive_end,
                    });
                    return Ok(wait_on_child_dependency(
                        state,
                        node_id,
                        child_node,
                        Vec::new(),
                    ));
                }
            },
            None => None,
        };
        let end = match end_child {
            Some(child_node) => match state.scheduler.node_state(child_node) {
                Some(EvalNodeState::Ready(value)) => Some(value.clone()),
                Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                _ => {
                    bytecode.pending = Some(BytecodePendingState::RangeJoin {
                        start_child,
                        end_child,
                        range_source: range_source.clone(),
                        inclusive_end,
                    });
                    return Ok(wait_on_child_dependency(
                        state,
                        node_id,
                        child_node,
                        Vec::new(),
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

    fn start_bytecode_map(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        map: &MapPlan,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                start_bytecode_map(state, node_id, parent_id, bytecode, map)
            });
        }
        continue_bytecode_map_entries(
            state,
            node_id,
            parent_id,
            bytecode,
            BytecodeMapState {
                entries: Vec::new(),
                key: None,
                value_expr: None,
                remaining_entries: map
                    .entries
                    .iter()
                    .map(|entry| (entry.key, entry.value))
                    .collect(),
                map_source: state.program.span_for_handle(map.handle),
            },
        )
    }

    fn continue_bytecode_map(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut map: BytecodeMapState,
        pending: MapResumeKind,
        resumed_value: Value,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_map(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    map,
                    pending,
                    resumed_value,
                )
            });
        }
        match pending {
            MapResumeKind::Key => {
                map.key = Some(resumed_value);
            }
            MapResumeKind::Value => {
                let key = map.key.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode map value resume is missing the current key",
                    )
                })?;
                map.value_expr = None;
                map.entries.push((key, resumed_value));
            }
        }
        continue_bytecode_map_entries(state, node_id, parent_id, bytecode, map)
    }

    fn continue_bytecode_map_entries(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        _parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut map: BytecodeMapState,
    ) -> Result<EvalStep, EvalError> {
        let mut remaining = map.remaining_entries.into_iter();
        loop {
            if map.key.is_none() {
                let Some((key_handle, value_handle)) = remaining.next() else {
                    break;
                };
                map.value_expr = Some(value_handle);
                match eval_dependent_bytecode_body_ref(state, node_id, key_handle, bytecode)? {
                    EvalStep::Ready(key) => map.key = Some(key),
                    EvalStep::Suspended(suspension) => {
                        map.remaining_entries = remaining.collect();
                        bytecode.pending = Some(BytecodePendingState::Map {
                            map,
                            pending: MapResumeKind::Key,
                        });
                        return Ok(EvalStep::Suspended(suspension));
                    }
                }
            }
            let key = map
                .key
                .take()
                .expect("map value evaluation should retain the current key");
            let value_handle = map
                .value_expr
                .take()
                .expect("map value evaluation should retain the current value expr");
            let value =
                match eval_dependent_bytecode_body_ref(state, node_id, value_handle, bytecode)? {
                    EvalStep::Ready(value) => value,
                    EvalStep::Suspended(suspension) => {
                        map.key = Some(key);
                        map.value_expr = Some(value_handle);
                        map.remaining_entries = remaining.collect();
                        bytecode.pending = Some(BytecodePendingState::Map {
                            map,
                            pending: MapResumeKind::Value,
                        });
                        return Ok(EvalStep::Suspended(suspension));
                    }
                };
            map.entries.push((key, value));
        }
        let provenance = Provenance::literal(map.map_source);
        let meta = aggregate_map_entries_meta(provenance, map.entries.iter());
        Ok(EvalStep::Ready(Value::map(map.entries).with_meta(meta)))
    }

    fn start_bytecode_interpolated_string(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        interpolated: &InterpolatedStringPlan,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                start_bytecode_interpolated_string(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    interpolated,
                )
            });
        }
        if state.queue_child_work {
            let parts = interpolated
                .parts
                .iter()
                .cloned()
                .map(|part| match part {
                    InterpolatedStringPlanPart::Text(text) => InterpolatedPart::Text(text),
                    InterpolatedStringPlanPart::Interpolation(body) => InterpolatedPart::Expr(body),
                })
                .collect::<Vec<_>>();
            let mut expr_nodes = Vec::new();
            for part in &parts {
                let InterpolatedPart::Expr(body) = part else {
                    continue;
                };
                expr_nodes.push(queue_child_prepared_entry(
                    state,
                    prepared_entry_from_bytecode(state.program, *body, bytecode)?,
                ));
            }
            if let Some(child_id) = expr_nodes.first().copied() {
                bytecode.pending = Some(BytecodePendingState::InterpolatedStringJoin {
                    parts: parts.clone(),
                    expr_nodes: expr_nodes.clone(),
                    string_source: state.program.span_for_handle(interpolated.handle),
                });
                return Ok(wait_on_child_dependency(
                    state,
                    node_id,
                    child_id,
                    Vec::new(),
                ));
            }
            return Ok(EvalStep::Ready(ready_interpolated_string_value(
                Vec::new(),
                state.program.span_for_handle(interpolated.handle),
            )));
        }
        continue_bytecode_interpolated_string_parts(
            state,
            node_id,
            parent_id,
            bytecode,
            BytecodeInterpolatedStringState {
                segments: Vec::new(),
                remaining_parts: interpolated
                    .parts
                    .iter()
                    .cloned()
                    .map(|part| match part {
                        InterpolatedStringPlanPart::Text(text) => InterpolatedPart::Text(text),
                        InterpolatedStringPlanPart::Interpolation(body) => {
                            InterpolatedPart::Expr(body)
                        }
                    })
                    .collect(),
                string_source: state.program.span_for_handle(interpolated.handle),
            },
        )
    }

    fn continue_bytecode_interpolated_string(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut string: BytecodeInterpolatedStringState,
        resumed_value: Value,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_interpolated_string(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    string,
                    resumed_value,
                )
            });
        }
        string
            .segments
            .push(interpolation_segment_text(&resumed_value)?);
        continue_bytecode_interpolated_string_parts(state, node_id, parent_id, bytecode, string)
    }

    fn continue_bytecode_interpolated_string_parts(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        _parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut string: BytecodeInterpolatedStringState,
    ) -> Result<EvalStep, EvalError> {
        let mut remaining = string.remaining_parts.into_iter();
        while let Some(part) = remaining.next() {
            match part {
                InterpolatedPart::Text(text) => string.segments.push(text.to_string()),
                InterpolatedPart::Expr(expr) => {
                    match eval_dependent_bytecode_body_ref(state, node_id, expr, bytecode)? {
                        EvalStep::Ready(value) => {
                            string.segments.push(interpolation_segment_text(&value)?)
                        }
                        EvalStep::Suspended(suspension) => {
                            string.remaining_parts = remaining.collect();
                            bytecode.pending =
                                Some(BytecodePendingState::InterpolatedString { string });
                            return Ok(EvalStep::Suspended(suspension));
                        }
                    }
                }
            }
        }
        Ok(EvalStep::Ready(ready_interpolated_string_value(
            string.segments,
            string.string_source,
        )))
    }

    fn continue_bytecode_interpolated_string_join(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        bytecode: &mut BytecodeState,
        parts: Vec<InterpolatedPart>,
        expr_nodes: Vec<EvalNodeId>,
        string_source: SourceSpan,
    ) -> Result<EvalStep, EvalError> {
        let _ = bytecode.acc.take();
        let mut segments = Vec::with_capacity(parts.len());
        let mut expr_index = 0usize;
        for part in &parts {
            match part {
                InterpolatedPart::Text(text) => segments.push(text.to_string()),
                InterpolatedPart::Expr(_) => {
                    let child_node = *expr_nodes
                        .get(expr_index)
                        .expect("interpolated string expr node count should match parts");
                    expr_index += 1;
                    match state.scheduler.node_state(child_node) {
                        Some(EvalNodeState::Ready(value)) => {
                            segments.push(interpolation_segment_text(value)?);
                        }
                        Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                        _ => {
                            bytecode.pending = Some(BytecodePendingState::InterpolatedStringJoin {
                                parts: parts.clone(),
                                expr_nodes: expr_nodes.clone(),
                                string_source: string_source.clone(),
                            });
                            return Ok(wait_on_child_dependency(
                                state,
                                node_id,
                                child_node,
                                Vec::new(),
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

    fn start_bytecode_struct(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        struct_literal: &StructLiteralPlan,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                start_bytecode_struct(state, node_id, parent_id, bytecode, struct_literal)
            });
        }
        let type_name = struct_literal_type_name(struct_literal.type_name.clone())?;
        let struct_source = state.program.span_for_handle(struct_literal.handle);
        let mut immediate_fields = Vec::new();
        let mut remaining_parts = Vec::new();
        for field in struct_literal.parts.iter() {
            match field {
                StructLiteralPlanPart::Field {
                    name,
                    expr: Some(expr),
                } => {
                    remaining_parts.push(StructLiteralPart::Field {
                        name: name.clone(),
                        expr: *expr,
                    });
                }
                StructLiteralPlanPart::Spread(expr) => {
                    remaining_parts.push(StructLiteralPart::Spread(*expr));
                }
                StructLiteralPlanPart::Field { name, expr: None } => {
                    let Some(value) = bytecode.scope.get(name.as_ref()) else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            format!("struct field {name} has no value"),
                        ));
                    };
                    set_struct_field(&mut immediate_fields, name.clone(), value.clone());
                }
            }
        }
        let joinable_fields = remaining_parts
            .iter()
            .map(|part| match part {
                StructLiteralPart::Field { name, expr } => Some((name.clone(), *expr)),
                StructLiteralPart::Spread(_) => None,
            })
            .collect::<Option<Vec<_>>>();
        if state.queue_child_work
            && immediate_fields.is_empty()
            && let Some(joinable_fields) = joinable_fields
            && !joinable_fields.is_empty()
        {
            let field_names = joinable_fields
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            let child_nodes = joinable_fields
                .iter()
                .map(|(_, expr)| {
                    prepared_entry_from_bytecode(state.program, *expr, bytecode)
                        .map(|prepared| queue_child_prepared_entry(state, prepared))
                })
                .collect::<Result<Vec<_>, _>>()?;
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
        continue_bytecode_struct_fields(
            state,
            node_id,
            parent_id,
            bytecode,
            BytecodeStructState {
                type_name,
                fields: immediate_fields,
                pending_part: None,
                remaining_parts,
                struct_source,
            },
        )
    }

    fn continue_bytecode_struct(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut struct_state: BytecodeStructState,
        resumed_value: Value,
    ) -> Result<EvalStep, EvalError> {
        if state.current_parent() != Some(node_id) {
            return state.with_active_node(node_id, |state| {
                continue_bytecode_struct(
                    state,
                    node_id,
                    parent_id,
                    bytecode,
                    struct_state,
                    resumed_value,
                )
            });
        }
        let part = struct_state.pending_part.take().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "bytecode struct resume is missing the current literal part",
            )
        })?;
        apply_struct_literal_part(
            &struct_state.type_name,
            &mut struct_state.fields,
            part,
            resumed_value,
        )?;
        continue_bytecode_struct_fields(state, node_id, parent_id, bytecode, struct_state)
    }

    fn continue_bytecode_struct_fields(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        _parent_id: Option<EvalNodeId>,
        bytecode: &mut BytecodeState,
        mut struct_state: BytecodeStructState,
    ) -> Result<EvalStep, EvalError> {
        let mut remaining = struct_state.remaining_parts.into_iter();
        loop {
            let Some(part) = struct_state
                .pending_part
                .take()
                .or_else(|| remaining.next())
            else {
                break;
            };
            let expr_handle = match &part {
                StructLiteralPart::Field { expr, .. } | StructLiteralPart::Spread(expr) => *expr,
            };
            match eval_dependent_bytecode_body_ref(state, node_id, expr_handle, bytecode)? {
                EvalStep::Ready(value) => apply_struct_literal_part(
                    &struct_state.type_name,
                    &mut struct_state.fields,
                    part,
                    value,
                )?,
                EvalStep::Suspended(suspension) => {
                    struct_state.pending_part = Some(part);
                    struct_state.remaining_parts = remaining.collect();
                    bytecode.pending = Some(BytecodePendingState::Struct { struct_state });
                    return Ok(EvalStep::Suspended(suspension));
                }
            }
        }
        let provenance = Provenance::literal(struct_state.struct_source);
        let meta = aggregate_child_meta(
            provenance,
            struct_state.fields.iter().map(|(_, value)| value),
        );
        Ok(EvalStep::Ready(
            Value::structure(struct_state.type_name, struct_state.fields).with_meta(meta),
        ))
    }

    fn continue_bytecode_struct_join(
        state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
        node_id: EvalNodeId,
        bytecode: &mut BytecodeState,
        type_name: Arc<str>,
        field_names: Vec<Arc<str>>,
        child_nodes: Vec<EvalNodeId>,
        struct_source: SourceSpan,
    ) -> Result<EvalStep, EvalError> {
        let _ = bytecode.acc.take();
        let mut fields = Vec::with_capacity(child_nodes.len());
        for (name, child_node) in field_names.iter().zip(child_nodes.iter()) {
            match state.scheduler.node_state(*child_node) {
                Some(EvalNodeState::Ready(value)) => fields.push((name.clone(), value.clone())),
                Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                _ => {
                    bytecode.pending = Some(BytecodePendingState::StructJoin {
                        type_name: type_name.clone(),
                        field_names: field_names.clone(),
                        child_nodes: child_nodes.clone(),
                        struct_source: struct_source.clone(),
                    });
                    return Ok(wait_on_child_dependency(
                        state,
                        node_id,
                        *child_node,
                        Vec::new(),
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

    let ops = Arc::clone(&state.program.resolve_bytecode_body(bytecode.body)?.ops);
    let source = bytecode.body.source();
    while bytecode.pc < ops.len() {
        if let Some(pending) = bytecode.pending.take() {
            let step = match pending {
                BytecodePendingState::Array { array } => {
                    let value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode array resumed without an item value",
                        )
                    })?;
                    continue_bytecode_array(state, node_id, parent_id, &mut bytecode, array, value)?
                }
                BytecodePendingState::ArrayJoin {
                    child_nodes,
                    array_source,
                } => continue_bytecode_array_join(
                    state,
                    node_id,
                    &mut bytecode,
                    child_nodes,
                    array_source,
                )?,
                BytecodePendingState::Tuple { tuple } => {
                    let value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode tuple resumed without an item value",
                        )
                    })?;
                    continue_bytecode_tuple(state, node_id, parent_id, &mut bytecode, tuple, value)?
                }
                BytecodePendingState::TupleJoin {
                    child_nodes,
                    tuple_source,
                } => continue_bytecode_tuple_join(
                    state,
                    node_id,
                    &mut bytecode,
                    child_nodes,
                    tuple_source,
                )?,
                BytecodePendingState::Range { range, pending } => {
                    let value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode range resumed without a bound value",
                        )
                    })?;
                    continue_bytecode_range(
                        state,
                        node_id,
                        parent_id,
                        &mut bytecode,
                        range,
                        pending,
                        value,
                    )?
                }
                BytecodePendingState::RangeJoin {
                    start_child,
                    end_child,
                    range_source,
                    inclusive_end,
                } => continue_bytecode_range_join(
                    state,
                    node_id,
                    &mut bytecode,
                    start_child,
                    end_child,
                    range_source,
                    inclusive_end,
                )?,
                BytecodePendingState::Map { map, pending } => {
                    let value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode map resumed without a key/value result",
                        )
                    })?;
                    continue_bytecode_map(
                        state,
                        node_id,
                        parent_id,
                        &mut bytecode,
                        map,
                        pending,
                        value,
                    )?
                }
                BytecodePendingState::Struct { struct_state } => {
                    let value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode struct resumed without a part value",
                        )
                    })?;
                    continue_bytecode_struct(
                        state,
                        node_id,
                        parent_id,
                        &mut bytecode,
                        struct_state,
                        value,
                    )?
                }
                BytecodePendingState::StructJoin {
                    type_name,
                    field_names,
                    child_nodes,
                    struct_source,
                } => continue_bytecode_struct_join(
                    state,
                    node_id,
                    &mut bytecode,
                    type_name,
                    field_names,
                    child_nodes,
                    struct_source,
                )?,
                BytecodePendingState::InterpolatedString { string } => {
                    let value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode interpolated string resumed without a segment value",
                        )
                    })?;
                    continue_bytecode_interpolated_string(
                        state,
                        node_id,
                        parent_id,
                        &mut bytecode,
                        string,
                        value,
                    )?
                }
                BytecodePendingState::InterpolatedStringJoin {
                    parts,
                    expr_nodes,
                    string_source,
                } => continue_bytecode_interpolated_string_join(
                    state,
                    node_id,
                    &mut bytecode,
                    parts,
                    expr_nodes,
                    string_source,
                )?,
                BytecodePendingState::Fallible { fallible_source } => {
                    let value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode fallible resumed without an operand value",
                        )
                    })?;
                    continue_bytecode_fallible(
                        state,
                        node_id,
                        &mut bytecode,
                        fallible_source,
                        None,
                        Some(Ok(value)),
                    )?
                }
                BytecodePendingState::Call { call, pending } => continue_bytecode_planned_call(
                    state,
                    node_id,
                    parent_id,
                    &mut bytecode,
                    call,
                    Some(pending),
                )?,
                BytecodePendingState::Match { match_state } => {
                    let scrutinee = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode match resumed without a scrutinee value",
                        )
                    })?;
                    continue_bytecode_match(
                        state,
                        node_id,
                        parent_id,
                        &mut bytecode,
                        match_state,
                        Some(scrutinee),
                    )?
                }
                BytecodePendingState::Where { where_state } => {
                    let keyword_value = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode where resumed without a keyword binding value",
                        )
                    })?;
                    continue_bytecode_where(
                        state,
                        node_id,
                        parent_id,
                        &mut bytecode,
                        where_state,
                        Some(keyword_value),
                    )?
                }
            };
            match step {
                EvalStep::Ready(value) => bytecode.acc = Some(value),
                EvalStep::Suspended(suspension) => {
                    return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                }
            }
            continue;
        }

        match &ops[bytecode.pc] {
            BytecodeOp::EvalReady(ready) => {
                bytecode.pc += 1;
                bytecode.acc = Some(state.with_source(source, |state| {
                    eval_inline_ready_bytecode_plan(
                        state,
                        node_id,
                        parent_id,
                        source,
                        *ready,
                        &bytecode.scope,
                    )
                })?);
            }
            BytecodeOp::LoadLocal {
                slot,
                source: name_source,
            } => {
                let value = bytecode
                    .locals
                    .get(*slot as usize)
                    .and_then(|value| value.clone())
                    .ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            format!("bytecode local slot {slot} is not initialized"),
                        )
                    })?;
                state.emit(
                    node_id,
                    parent_id,
                    Some(name_source.clone()),
                    EvalEventKind::EnterExpr {
                        kind: EvalExprKind::NameRef,
                    },
                );
                state.emit(
                    node_id,
                    parent_id,
                    Some(name_source.clone()),
                    EvalEventKind::ExitReady {
                        reuse: value.meta.reuse.clone(),
                        crossing: value.meta.crossing.clone(),
                    },
                );
                bytecode.pc += 1;
                bytecode.acc = Some(value);
            }
            BytecodeOp::EvalStaticMember {
                field_source,
                static_member,
            } => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(field_source.clone()),
                    EvalEventKind::EnterExpr {
                        kind: EvalExprKind::Field,
                    },
                );
                let value =
                    static_member_value_from_source(state, field_source.clone(), static_member)?;
                state.emit(
                    node_id,
                    parent_id,
                    Some(field_source.clone()),
                    EvalEventKind::ExitReady {
                        reuse: value.meta.reuse.clone(),
                        crossing: value.meta.crossing.clone(),
                    },
                );
                bytecode.pc += 1;
                bytecode.acc = Some(value);
            }
            BytecodeOp::EvalBytecodeBody(body) => {
                bytecode.pc += 1;
                let bytecode_body = state.program.resolve_bytecode_body(*body)?;
                let nested = BytecodeState {
                    scope: bytecode.scope.clone(),
                    locals: init_bytecode_locals_from_parent(
                        state.program,
                        bytecode_body,
                        &bytecode,
                    )?,
                    last: Value::unit(),
                    body: *body,
                    pc: 0,
                    acc: None,
                    temp: None,
                    pending: None,
                };
                match state.with_source(body.source(), |state| {
                    continue_bytecode_body(state, node_id, parent_id, nested)
                })? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartArray(array) => {
                bytecode.pc += 1;
                match start_bytecode_array(state, node_id, parent_id, &mut bytecode, array)? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartTuple(tuple) => {
                bytecode.pc += 1;
                match start_bytecode_tuple(state, node_id, parent_id, &mut bytecode, tuple)? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartRange(range) => {
                bytecode.pc += 1;
                match start_bytecode_range(state, node_id, parent_id, &mut bytecode, range)? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartMap(map) => {
                bytecode.pc += 1;
                match start_bytecode_map(state, node_id, parent_id, &mut bytecode, map)? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartInterpolatedString(interpolated) => {
                bytecode.pc += 1;
                match start_bytecode_interpolated_string(
                    state,
                    node_id,
                    parent_id,
                    &mut bytecode,
                    interpolated,
                )? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartStructLiteral(struct_literal) => {
                bytecode.pc += 1;
                match start_bytecode_struct(
                    state,
                    node_id,
                    parent_id,
                    &mut bytecode,
                    struct_literal,
                )? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartCall(call) => {
                bytecode.pc += 1;
                let step = continue_bytecode_planned_call(
                    state,
                    node_id,
                    parent_id,
                    &mut bytecode,
                    BytecodePlannedCallState {
                        plan_source: call.handle.source(),
                        call_source: state.program.span_for_handle(call.handle),
                        callee_plan: call.callee.clone(),
                        type_args: Arc::clone(&call.type_args),
                        positional_args: Arc::clone(&call.positional_args),
                        callee: None,
                        args: Vec::new(),
                        next_arg_index: 0,
                        queued_arg_nodes: Vec::new(),
                    },
                    None,
                )?;
                match step {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartFallible(fallible) => {
                bytecode.pc += 1;
                let step = continue_bytecode_fallible(
                    state,
                    node_id,
                    &mut bytecode,
                    state.program.span_for_handle(fallible.handle),
                    fallible.expr,
                    None,
                )?;
                match step {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartMatch(match_plan) => {
                bytecode.pc += 1;
                let step = continue_bytecode_match(
                    state,
                    node_id,
                    parent_id,
                    &mut bytecode,
                    BytecodeMatchState {
                        match_plan: match_plan.clone(),
                    },
                    None,
                )?;
                match step {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::StartWhere(where_plan) => {
                bytecode.pc += 1;
                let where_state = init_bytecode_where_state(state, where_plan, &bytecode.scope)?;
                let step = continue_bytecode_where(
                    state,
                    node_id,
                    parent_id,
                    &mut bytecode,
                    where_state,
                    None,
                )?;
                match step {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::InternalError { source, message } => {
                bytecode.pc += 1;
                let _ = source;
                return Err(EvalError::new(EvalErrorKind::Internal, message.clone()));
            }
            BytecodeOp::EvalIfBranches {
                then_branch,
                else_branch,
            } => {
                bytecode.pc += 1;
                let condition = bytecode.acc.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode if-branches has no pending condition value",
                    )
                })?;
                let ValueRep::Bool(condition) = condition.rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "if condition must evaluate to Bool",
                    ));
                };
                let branch = if condition {
                    *then_branch
                } else {
                    *else_branch
                };
                match state.with_source(branch.expr_id().source(), |state| {
                    eval_dependent_bytecode_body_ref(state, node_id, branch, &bytecode)
                })? {
                    EvalStep::Ready(value) => bytecode.acc = Some(value),
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::ApplyPrefix(operator) => {
                let value = bytecode.acc.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode prefix has no pending operand value",
                    )
                })?;
                bytecode.acc = Some(apply_prefix_operator(*operator, value)?);
                bytecode.pc += 1;
            }
            BytecodeOp::EvalBinary { operator, rhs } => {
                if let Some(lhs) = bytecode.temp.take() {
                    let rhs = bytecode.acc.take().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            "bytecode binary has no pending rhs value",
                        )
                    })?;
                    bytecode.acc = Some(apply_binary_operator(lhs, *operator, rhs)?);
                    bytecode.pc += 1;
                    continue;
                }

                let lhs = bytecode.acc.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode binary has no pending lhs value",
                    )
                })?;
                if let Some(short_circuit) = short_circuit_binary(&lhs, *operator)? {
                    bytecode.acc = Some(short_circuit);
                    bytecode.pc += 1;
                    continue;
                }

                bytecode.temp = Some(lhs);
                match state.with_source(rhs.expr_id().source(), |state| {
                    eval_dependent_bytecode_body_ref(state, node_id, *rhs, &bytecode)
                })? {
                    EvalStep::Ready(rhs) => {
                        let lhs = bytecode.temp.take().expect("binary lhs should be staged");
                        bytecode.acc = Some(apply_binary_operator(lhs, *operator, rhs)?);
                        bytecode.pc += 1;
                    }
                    EvalStep::Suspended(suspension) => {
                        return Ok(suspend_bytecode(suspension, node_id, parent_id, bytecode));
                    }
                }
            }
            BytecodeOp::ProjectField {
                field_name,
                field_source,
            } => {
                let receiver = bytecode.acc.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode field projection has no pending receiver value",
                    )
                })?;
                bytecode.acc = Some(project_field_or_method_value(
                    state,
                    receiver,
                    Arc::clone(field_name),
                    field_source.clone(),
                )?);
                bytecode.pc += 1;
            }
            BytecodeOp::RaiseUserFail { fail_source } => {
                let value = bytecode.acc.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode fail has no pending failure value",
                    )
                })?;
                let error = user_fail_error(value);
                state.emit(
                    node_id,
                    parent_id,
                    Some(fail_source.clone()),
                    EvalEventKind::EnterExpr {
                        kind: EvalExprKind::Fail,
                    },
                );
                state.emit(
                    node_id,
                    parent_id,
                    Some(fail_source.clone()),
                    EvalEventKind::ExitPoison {
                        kind: error.kind,
                        message: error.message.clone(),
                    },
                );
                return Err(error);
            }
            BytecodeOp::BindLet {
                pattern,
                local_bindings,
            } => {
                let value = bytecode.acc.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode let binding has no pending value",
                    )
                })?;
                bind_let_pattern(&mut bytecode.scope, pattern, value)?;
                for (name, slot) in local_bindings.iter() {
                    let value = bytecode.scope.get(name.as_ref()).cloned().ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            format!("bytecode let binding {name} was not written to scope"),
                        )
                    })?;
                    bytecode.locals[*slot as usize] = Some(value);
                }
                bytecode.pc += 1;
            }
            BytecodeOp::BindFnDecl {
                function,
                local_slot,
            } => {
                let (name, _name_source, value) = state.with_source(source, |state| {
                    bind_named_function_decl(state, function, &bytecode.scope)
                })?;
                env_mut(&mut bytecode.scope).insert(name, value.clone());
                if let Some(slot) = local_slot {
                    bytecode.locals[*slot as usize] = Some(value);
                }
                bytecode.pc += 1;
            }
            BytecodeOp::SetLast => {
                bytecode.last = bytecode.acc.take().ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "bytecode set-last has no pending value",
                    )
                })?;
                bytecode.pc += 1;
            }
            BytecodeOp::ReturnLast => return Ok(EvalStep::Ready(bytecode.last)),
        }
    }
    Ok(EvalStep::Ready(bytecode.last))
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

fn decode_string_literal(text: &str) -> Result<String, EvalError> {
    let inner = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("invalid string literal token: {text}"),
            )
        })?;
    let mut decoded = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escaped = chars.next().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("unterminated string escape in literal: {text}"),
            )
        })?;
        match escaped {
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            '"' => decoded.push('"'),
            '\\' => decoded.push('\\'),
            other => decoded.push(other),
        }
    }
    Ok(decoded)
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

pub(super) fn queue_child_body_ref(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    body: ExprBodyRef,
    env: &EvalEnv,
) -> EvalNodeId {
    queue_child_expr_body(
        state,
        PreparedBodyRef::from_body_ref(state.program, body)
            .expect("queued child body ref should resolve to an execution body"),
        env,
    )
}

fn queue_child_expr_body(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    body: PreparedBodyRef,
    env: &EvalEnv,
) -> EvalNodeId {
    let child_id = state.alloc_node_id();
    let body_kind = prepared_body_kind(state.program, &body);
    let resolved_source = state
        .program
        .span_for_expr_id(body.root_expr_id())
        .expect("queued child expression handle should have a source span");
    state.emit_current_dependency(child_id, Some(resolved_source.clone()));
    state.scheduler.record_node(EvalNodeRecord {
        node_id: child_id,
        parent_id: state.current_parent(),
        kind: EvalNodeKind::Expr { kind: body_kind },
        source: Some(resolved_source),
    });
    let locals = init_bytecode_locals_from_scope(
        state
            .program
            .resolve_bytecode_body(body.body)
            .expect("queued child body should resolve to bytecode"),
        env,
    );
    let scheduled = ScheduledExpr {
        root_expr_id: body.root_expr_id(),
        kind: ScheduledExprKind::Prepared(body),
        env: env.clone(),
        locals,
        call_stack: if state.track_calls {
            state.call_stack.clone()
        } else {
            Vec::new()
        },
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

pub(super) fn queue_child_prepared_entry(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    prepared: PreparedEntry,
) -> EvalNodeId {
    let child_id = state.alloc_node_id();
    let body_kind = prepared_body_kind(state.program, &prepared.body);
    let resolved_source = state
        .program
        .span_for_expr_id(prepared.body.root_expr_id())
        .expect("prepared entry body should resolve to a source span");
    state.emit_current_dependency(child_id, Some(resolved_source.clone()));
    state.scheduler.record_node(EvalNodeRecord {
        node_id: child_id,
        parent_id: state.current_parent(),
        kind: EvalNodeKind::Expr { kind: body_kind },
        source: Some(resolved_source),
    });
    let scheduled = ScheduledExpr {
        root_expr_id: prepared.body.root_expr_id(),
        kind: ScheduledExprKind::Prepared(prepared.body),
        env: prepared.env,
        locals: prepared.locals,
        call_stack: if state.track_calls {
            state.call_stack.clone()
        } else {
            Vec::new()
        },
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
        call_stack: state.call_stack.clone(),
    })
}

fn set_struct_field(fields: &mut Vec<(Arc<str>, Value)>, name: Arc<str>, value: Value) {
    if let Some((_, existing)) = fields
        .iter_mut()
        .find(|(field_name, _)| *field_name == name)
    {
        *existing = value;
    } else {
        fields.push((name, value));
    }
}

fn apply_struct_literal_part(
    type_name: &Arc<str>,
    fields: &mut Vec<(Arc<str>, Value)>,
    part: StructLiteralPart,
    value: Value,
) -> Result<(), EvalError> {
    match part {
        StructLiteralPart::Field { name, .. } => {
            set_struct_field(fields, name, value);
            Ok(())
        }
        StructLiteralPart::Spread(_) => {
            let ValueRep::Struct(struct_value) = value.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "struct spread requires a struct value",
                ));
            };
            if struct_value.type_name != *type_name {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!(
                        "struct spread expected {}, got {}",
                        type_name, struct_value.type_name
                    ),
                ));
            }
            for (field_name, field_value) in struct_value.fields.iter().cloned() {
                set_struct_field(fields, field_name, field_value);
            }
            Ok(())
        }
    }
}

pub(super) fn project_struct_field(
    receiver: Value,
    field_name: Arc<str>,
) -> Result<Value, EvalError> {
    let ValueRep::Struct(StructValue { fields, .. }) = &receiver.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("cannot project field {field_name} from non-struct value"),
        ));
    };
    let value = fields
        .iter()
        .find_map(|(name, value)| (name.as_ref() == field_name.as_ref()).then(|| value.clone()))
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::TypeError,
                format!("struct field {field_name} was not found"),
            )
        })?;
    Ok(value.project(ProjectionStep::Field(field_name.to_string())))
}

pub(super) fn apply_binary_operator(
    lhs: Value,
    operator: SyntaxKind,
    rhs: Value,
) -> Result<Value, EvalError> {
    if let Some(path_join) = apply_path_join_operator(&lhs, operator, &rhs)? {
        return Ok(path_join);
    }

    if let Some(arithmetic) = apply_u64_arithmetic_operator(&lhs.rep, operator, &rhs.rep)? {
        return Ok(arithmetic);
    }

    let equals = values_equal(&lhs.rep, &rhs.rep)?;
    let ordering = values_order(&lhs.rep, &rhs.rep);
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
        SyntaxKind::LT => Ok(Value::bool(matches!(ordering?, std::cmp::Ordering::Less))),
        SyntaxKind::LT_EQ => Ok(Value::bool(matches!(
            ordering?,
            std::cmp::Ordering::Less | std::cmp::Ordering::Equal
        ))),
        SyntaxKind::GT => Ok(Value::bool(matches!(
            ordering?,
            std::cmp::Ordering::Greater
        ))),
        SyntaxKind::GT_EQ => Ok(Value::bool(matches!(
            ordering?,
            std::cmp::Ordering::Greater | std::cmp::Ordering::Equal
        ))),
        _ => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("binary operator {operator:?} is not supported yet"),
        )),
    }
}

fn apply_path_join_operator(
    lhs: &Value,
    operator: SyntaxKind,
    rhs: &Value,
) -> Result<Option<Value>, EvalError> {
    if operator != SyntaxKind::SLASH {
        return Ok(None);
    }

    let Some(lhs_path) = value_path_descriptor(&lhs.rep) else {
        return Ok(None);
    };

    let rhs_segment = match value_path_descriptor(&rhs.rep) {
        Some(path) => path,
        None => match &rhs.rep {
            ValueRep::String(text) => text.clone(),
            _ => {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "right operand of / must be String or Path when left operand is Path",
                ));
            }
        },
    };

    let joined = std::path::PathBuf::from(lhs_path.as_ref())
        .join(rhs_segment.as_ref())
        .to_string_lossy()
        .into_owned();
    Ok(Some(
        Value::external("Path", joined)
            .with_meta(lhs.meta.clone())
            .project(ProjectionStep::Path(rhs_segment.to_string())),
    ))
}

fn value_path_descriptor(value: &ValueRep) -> Option<Arc<str>> {
    match value {
        ValueRep::External(external) if external.kind.as_ref() == "Path" => {
            Some(external.descriptor.clone())
        }
        ValueRep::Struct(struct_value) if struct_value.type_name.as_ref() == "Path" => struct_value
            .fields
            .iter()
            .find_map(|(name, value)| match (name.as_ref(), &value.rep) {
                ("path", ValueRep::String(path)) => Some(path.clone()),
                _ => None,
            }),
        _ => None,
    }
}

fn apply_u64_arithmetic_operator(
    lhs: &ValueRep,
    operator: SyntaxKind,
    rhs: &ValueRep,
) -> Result<Option<Value>, EvalError> {
    let (ValueRep::U64(lhs), ValueRep::U64(rhs)) = (lhs, rhs) else {
        return Ok(None);
    };

    let value = match operator {
        SyntaxKind::PLUS => lhs.checked_add(*rhs).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "u64 addition overflowed at runtime",
            )
        })?,
        SyntaxKind::MINUS => lhs.checked_sub(*rhs).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "u64 subtraction underflowed at runtime",
            )
        })?,
        SyntaxKind::STAR => lhs.checked_mul(*rhs).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "u64 multiplication overflowed at runtime",
            )
        })?,
        SyntaxKind::SLASH => {
            if *rhs == 0 {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "u64 division by zero at runtime",
                ));
            }
            lhs / rhs
        }
        SyntaxKind::PERCENT => {
            if *rhs == 0 {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "u64 remainder by zero at runtime",
                ));
            }
            lhs % rhs
        }
        _ => return Ok(None),
    };

    Ok(Some(Value::u64(value)))
}

fn values_order(lhs: &ValueRep, rhs: &ValueRep) -> Result<std::cmp::Ordering, EvalError> {
    Ok(compare_value_reps(lhs, rhs))
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
        (ValueRep::External(lhs), ValueRep::External(rhs)) => Ok(lhs == rhs),
        (ValueRep::External(_), _) | (_, ValueRep::External(_)) => Ok(false),
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
