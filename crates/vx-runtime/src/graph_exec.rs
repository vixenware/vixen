use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vx_exec_protocol::{ExecCommand, ObserveRequest};
use vx_ir::build::VirProgram;
use vx_ir::ids::{BodyId, PatternId};
use vx_ir::pattern::PatternKind;
use vx_runtime_core::{CoreFailureContext, CorePoison, NodeOutput, UnsupportedNode};
use vx_runtime_driver::{
    BoundaryDemand, DemandRequest, Driver, DriverError, PublicValueDemand, ResolvedNode,
};
use vx_runtime_graph::{ConstValue, HostMethod, NodeId, NodeKind};
use vx_types::TypeEnv;

use crate::ability::{AttrLit, CmpOp, Constraint, ProviderRegistry};
use crate::analyze::{
    CapabilityRequestMeta, CliArgPiece, CliArgTemplate, ExecInvocationMeta, GraphRuntimeMeta,
};
use crate::cache::{
    CachedExecOutcome, ExecInvocationCacheKey, HostExecCacheKey, ObserverCacheIdentity,
    RuntimeCache, RuntimeCacheBackend,
};
use crate::eval::{FailureContext, Outcome, Poison, Thunk, Value, from_wire};
use crate::remote::{Part, PartKey};
use crate::strand::{HandleId, ObserveClientId, ObserveOutcome, dispatch_observe, force_remote};

type SharedDriver = Rc<RefCell<Driver>>;
type ExternalThunks = Rc<RefCell<HashMap<NodeId, Thunk>>>;

#[derive(Clone)]
struct RuntimeBoundaries {
    exec_client: Option<ObserveClientId>,
    providers: ProviderRegistry,
    meta: Rc<GraphRuntimeMeta>,
    remote_trees: Rc<RefCell<HashMap<NodeId, HandleId>>>,
    cache: Rc<RuntimeCache>,
}

impl Default for RuntimeBoundaries {
    fn default() -> Self {
        Self {
            exec_client: None,
            providers: ProviderRegistry::default(),
            meta: Rc::new(GraphRuntimeMeta::default()),
            remote_trees: Rc::new(RefCell::new(HashMap::new())),
            cache: Rc::new(RuntimeCache::new()),
        }
    }
}

impl RuntimeBoundaries {
    fn with_cache_backend(backend: Box<dyn RuntimeCacheBackend>) -> Self {
        Self {
            cache: Rc::new(RuntimeCache::with_backend(backend)),
            ..Self::default()
        }
    }
}

#[derive(Debug)]
struct ToolRequest {
    command_package: String,
    command_name: String,
    binary: String,
    declared_fingerprint: Option<String>,
}

pub(crate) fn run_source(source: &str, entry: &str) -> Result<(Outcome, u64), String> {
    let graph = crate::analyze::analyze_source(source, entry)?;
    drive_graph(
        graph,
        Vec::new(),
        HashMap::new(),
        RuntimeBoundaries::default(),
    )
}

pub(crate) fn run_body(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
) -> Result<(Outcome, u64), String> {
    run_body_with_boundaries(program, env, body, args, RuntimeBoundaries::default())
}

pub(crate) fn run_body_with_cache_backend(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    backend: Box<dyn RuntimeCacheBackend>,
) -> Result<(Outcome, u64), String> {
    run_body_with_boundaries(
        program,
        env,
        body,
        args,
        RuntimeBoundaries::with_cache_backend(backend),
    )
}

pub(crate) fn run_body_with_exec(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    exec_client: ObserveClientId,
) -> Result<(Outcome, u64), String> {
    run_body_with_boundaries(
        program,
        env,
        body,
        args,
        RuntimeBoundaries {
            exec_client: Some(exec_client),
            ..RuntimeBoundaries::default()
        },
    )
}

pub(crate) fn run_body_with_exec_and_cache_backend(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    exec_client: ObserveClientId,
    backend: Box<dyn RuntimeCacheBackend>,
) -> Result<(Outcome, u64), String> {
    run_body_with_boundaries(
        program,
        env,
        body,
        args,
        RuntimeBoundaries {
            exec_client: Some(exec_client),
            ..RuntimeBoundaries::with_cache_backend(backend)
        },
    )
}

pub(crate) fn run_body_with_exec_and_providers(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    exec_client: ObserveClientId,
    providers: ProviderRegistry,
) -> Result<(Outcome, u64), String> {
    run_body_with_boundaries(
        program,
        env,
        body,
        args,
        RuntimeBoundaries {
            exec_client: Some(exec_client),
            providers,
            ..RuntimeBoundaries::default()
        },
    )
}

fn run_body_with_boundaries(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mut runtime: RuntimeBoundaries,
) -> Result<(Outcome, u64), String> {
    let (mut graph, _, _, binding_nodes, meta) =
        crate::analyze::build_graph_indexed_with_meta(program, env, body);
    runtime.meta = Rc::new(meta);
    let params: Vec<PatternId> = program.arena[body]
        .params
        .iter()
        .map(|param| param.pattern)
        .collect();
    if params.len() != args.len() {
        return Err(format!(
            "graph body argument count mismatch: expected {}, got {}",
            params.len(),
            args.len()
        ));
    }
    let mut seeds = Vec::new();
    let mut externals = HashMap::new();
    for (pattern, value) in params.into_iter().zip(args) {
        seed_pattern(
            program,
            pattern,
            value,
            &binding_nodes,
            &mut graph,
            &mut externals,
            &runtime,
            &mut seeds,
        )?;
    }
    drive_graph(graph, seeds, externals, runtime)
}

fn drive_graph(
    graph: vx_runtime_graph::DemandGraph,
    seeds: Vec<(NodeId, NodeOutput)>,
    externals: HashMap<NodeId, Thunk>,
    runtime: RuntimeBoundaries,
) -> Result<(Outcome, u64), String> {
    let mut driver = Driver::new(graph);
    if runtime.cache.has_persistent_layer() {
        driver = driver.with_call_cache(Rc::clone(&runtime.cache).function_output_call_cache());
    }
    for (node, output) in seeds {
        driver = driver.with_seed(node, output);
    }
    let driver = Rc::new(RefCell::new(driver));
    let externals = Rc::new(RefCell::new(externals));
    let request = driver.borrow().root_request();
    let result = drive_request(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        request,
    )?;
    let kernel_executions = driver.borrow().kernel_executions();
    let outcome = match result {
        Ok(resolved) => Outcome::Clean(value_from_resolved(
            resolved,
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
        )?),
        Err(DriverError::Poison(poison)) => Outcome::Poisoned {
            reason: poison.reason,
            span: poison.span,
        },
        Err(DriverError::Unsupported(unsupported)) => {
            return Err(format!(
                "graph unsupported at node {:?}: {}",
                unsupported.node, unsupported.reason
            ));
        }
        Err(DriverError::Boundary(boundary)) => {
            let node = boundary.node();
            return Err(format!(
                "graph boundary at node {node:?}: boundary dispatch is not wired yet"
            ));
        }
        Err(DriverError::Stalled(node)) => {
            return Err(format!("graph stalled at node {node:?}"));
        }
    };
    Ok((outcome, kernel_executions))
}

fn drive_request(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    request: DemandRequest,
) -> Result<Result<ResolvedNode, DriverError>, String> {
    loop {
        let result = {
            let mut driver = driver.borrow_mut();
            driver.drive(request)
        };
        match result {
            Err(DriverError::Boundary(boundary)) => {
                if materialize_boundary(
                    Rc::clone(&driver),
                    Rc::clone(&externals),
                    runtime.clone(),
                    &boundary,
                )? {
                    continue;
                }
                return Ok(Err(DriverError::Boundary(boundary)));
            }
            other => return Ok(other),
        }
    }
}

fn materialize_boundary(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
) -> Result<bool, String> {
    let node = boundary.node();
    if externals.borrow().contains_key(&node) {
        materialize_external_boundary(driver, externals, runtime, node)?;
        return Ok(true);
    }

    let exec = runtime.meta.execs.get(&node).cloned();
    if let Some(exec) = exec {
        materialize_exec_invocation(driver, externals, runtime, boundary, &exec)?;
        return Ok(true);
    }

    if runtime.meta.tree_files.contains_key(&node) {
        materialize_tree_file(driver, externals, runtime, boundary)?;
        return Ok(true);
    }

    let capability = runtime.meta.capabilities.get(&node).cloned();
    if let Some(capability) = capability {
        materialize_capability_request(driver, externals, runtime, node, &capability)?;
        return Ok(true);
    }

    let host_method = {
        let driver = driver.borrow();
        match driver.node(node).map(|node| &node.kind) {
            Some(NodeKind::HostMethod { method }) => Some(*method),
            _ => None,
        }
    };
    let Some(method) = host_method else {
        return Ok(false);
    };

    materialize_host_method(driver, externals, runtime, boundary, method)?;
    Ok(true)
}

fn materialize_external_boundary(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    node: NodeId,
) -> Result<(), String> {
    let thunk = externals
        .borrow_mut()
        .remove(&node)
        .ok_or_else(|| format!("graph external boundary {node:?} has no thunk"))?;
    let value = thunk
        .materialize_for_graph_boundary()
        .map_err(|poison| poison.message())?;
    let output = {
        let mut driver = driver.borrow_mut();
        node_output_from_value_in_driver(&mut driver, Rc::clone(&externals), runtime, node, value)?
    };
    driver.borrow_mut().seed(node, output);
    Ok(())
}

fn materialize_exec_invocation(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    invocation: &ExecInvocationMeta,
) -> Result<(), String> {
    let node = boundary.node();
    let client = runtime.exec_client.ok_or_else(|| {
        "a command invocation must run on a strand with an observe executor registered \
         (no executor available)"
            .to_string()
    })?;
    let Some((lead, rest)) = invocation.args.split_first() else {
        return Err("empty command invocation".into());
    };
    let tool_node = command_tool_node(lead)?;
    let tool = demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        tool_node,
    )?;
    let tool = tool_request(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        tool,
    )?;
    let mut args = Vec::with_capacity(rest.len());
    for arg in rest {
        args.push(render_cli_arg(
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
            boundary,
            arg,
        )?);
    }
    let mut output_roots = Vec::with_capacity(invocation.writable_roots.len());
    for root in &invocation.writable_roots {
        let output = demand_payload(
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
            boundary,
            *root,
        )?;
        output_roots.push(expect_string(output, "exec writable root")?);
    }

    let program_fingerprint = crate::host::fingerprint_executable(&tool.binary)?;
    let cache_key = exec_invocation_key(
        &tool,
        program_fingerprint,
        args.clone(),
        output_roots.clone(),
        invocation,
    );
    if let Some(cached) = runtime.cache.get_exec_invocation(&cache_key) {
        seed_exec_outcome(driver, externals, runtime, node, cached)?;
        return Ok(());
    }

    let observed = dispatch_observe(
        client,
        ObserveRequest {
            observer: invocation.observer.clone(),
            command: ExecCommand {
                program: tool.binary,
                args: args.clone(),
            },
            outputs: output_roots,
        },
    );
    let cacheable = cacheable_from_observed(&observed)?;
    runtime
        .cache
        .put_exec_invocation(cache_key, cacheable.clone());
    seed_exec_outcome(driver, externals, runtime, node, cacheable)?;
    Ok(())
}

fn cacheable_from_observed(observed: &ObserveOutcome) -> Result<CachedExecOutcome, String> {
    match observed {
        ObserveOutcome::Value(wire) => Ok(CachedExecOutcome::Value(wire.clone())),
        ObserveOutcome::Tree(handle) => Ok(CachedExecOutcome::Tree(*handle)),
        ObserveOutcome::Poisoned(reason) => Err(reason.clone()),
    }
}

fn seed_exec_outcome(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    node: NodeId,
    outcome: CachedExecOutcome,
) -> Result<(), String> {
    let output = {
        let mut driver = driver.borrow_mut();
        match outcome {
            CachedExecOutcome::Value(wire) => {
                let value = from_wire(&wire).map_err(|poison| poison.message())?;
                node_output_from_value_in_driver(
                    &mut driver,
                    Rc::clone(&externals),
                    runtime.clone(),
                    node,
                    value,
                )?
            }
            CachedExecOutcome::Tree(handle) => {
                runtime.remote_trees.borrow_mut().insert(node, handle);
                NodeOutput::OutputTree
            }
        }
    };
    driver.borrow_mut().seed(node, output);
    Ok(())
}

fn command_tool_node(arg: &CliArgTemplate) -> Result<NodeId, String> {
    match arg.pieces.as_slice() {
        [CliArgPiece::Node(node)] => Ok(*node),
        _ => Err("a command invocation must lead with a single `$tool` hole".into()),
    }
}

fn tool_request(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    tool: NodeOutput,
) -> Result<ToolRequest, String> {
    let NodeOutput::Struct {
        field_names,
        fields,
    } = tool
    else {
        return Err(format!(
            "the leading hole of a command invocation must be a tool, got {tool:?}"
        ));
    };
    let command = field_names
        .iter()
        .position(|name| name == "command")
        .and_then(|index| fields.get(index).copied())
        .ok_or_else(|| "tool value has no `command` field".to_string())?;
    let binary = field_names
        .iter()
        .position(|name| name == "binary")
        .and_then(|index| fields.get(index).copied())
        .ok_or_else(|| "tool value has no `binary` field".to_string())?;
    let declared_fingerprint = field_names
        .iter()
        .position(|name| name == "fingerprint")
        .and_then(|index| fields.get(index).copied());
    let command = demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        command,
    )?;
    let (command_package, command_name) = command_coordinate(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        command,
    )?;
    let declared_fingerprint = declared_fingerprint
        .map(|fingerprint| {
            expect_string(
                demand_payload(
                    Rc::clone(&driver),
                    Rc::clone(&externals),
                    runtime.clone(),
                    boundary,
                    fingerprint,
                )?,
                "tool `fingerprint`",
            )
        })
        .transpose()?;
    let binary = demand_payload(driver, externals, runtime, boundary, binary)?;
    let binary = expect_string(binary, "tool `binary`")?;
    Ok(ToolRequest {
        command_package,
        command_name,
        binary,
        declared_fingerprint,
    })
}

fn command_coordinate(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    command: NodeOutput,
) -> Result<(String, String), String> {
    let NodeOutput::Struct {
        field_names,
        fields,
    } = command
    else {
        return Err(format!(
            "tool `command` must be a grammar coordinate, got {command:?}"
        ));
    };
    let package = field_names
        .iter()
        .position(|name| name == "package")
        .and_then(|index| fields.get(index).copied())
        .ok_or_else(|| "tool `command` has no `package` field".to_string())?;
    let name = field_names
        .iter()
        .position(|field| field == "name")
        .and_then(|index| fields.get(index).copied())
        .ok_or_else(|| "tool `command` has no `name` field".to_string())?;
    let package = expect_string(
        demand_payload(
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
            boundary,
            package,
        )?,
        "tool `command.package`",
    )?;
    let name = expect_string(
        demand_payload(driver, externals, runtime, boundary, name)?,
        "tool `command.name`",
    )?;
    Ok((package, name))
}

fn exec_invocation_key(
    tool: &ToolRequest,
    program_fingerprint: String,
    args: Vec<String>,
    output_roots: Vec<String>,
    invocation: &ExecInvocationMeta,
) -> ExecInvocationCacheKey {
    let observer = match &invocation.observer {
        Some(bundle) => ObserverCacheIdentity::Bundle(bundle.content_identity()),
        None => ObserverCacheIdentity::Default,
    };
    ExecInvocationCacheKey {
        command_package: tool.command_package.clone(),
        command_name: tool.command_name.clone(),
        program: tool.binary.clone(),
        program_fingerprint,
        declared_tool_fingerprint: tool.declared_fingerprint.clone(),
        args,
        output_roots,
        observer,
    }
}

fn render_cli_arg(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    arg: &CliArgTemplate,
) -> Result<String, String> {
    let mut rendered = String::new();
    for piece in &arg.pieces {
        match piece {
            CliArgPiece::Text(text) => rendered.push_str(text),
            CliArgPiece::Node(node) => {
                let output = demand_payload(
                    Rc::clone(&driver),
                    Rc::clone(&externals),
                    runtime.clone(),
                    boundary,
                    *node,
                )?;
                match output {
                    NodeOutput::Str(value) => rendered.push_str(&value),
                    NodeOutput::U64(value) => rendered.push_str(&value.to_string()),
                    other => {
                        return Err(format!("can't render {other:?} as a CLI argument"));
                    }
                }
            }
        }
    }
    Ok(rendered)
}

fn materialize_tree_file(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
) -> Result<(), String> {
    let node = boundary.node();
    let inputs = {
        let driver = driver.borrow();
        driver
            .node(node)
            .ok_or_else(|| format!("tree.file graph boundary {node:?} has no node"))?
            .inputs
            .clone()
    };
    let [receiver, path] = inputs.as_slice() else {
        return Err(format!(
            "tree.file graph node expects receiver and path, got {} inputs",
            inputs.len()
        ));
    };
    let receiver_output = demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        *receiver,
    )?;
    if !matches!(receiver_output, NodeOutput::OutputTree) {
        return Err(format!(
            "tree.file receiver must be a producing Tree, got {receiver_output:?}"
        ));
    }
    let handle = runtime
        .remote_trees
        .borrow()
        .get(receiver)
        .copied()
        .ok_or_else(|| format!("tree.file receiver node {receiver:?} has no remote handle"))?;
    let path = expect_string(
        demand_payload(
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime,
            boundary,
            *path,
        )?,
        "tree.file path",
    )?;
    let value = match &*force_remote(handle, PartKey::TreePath(path.clone())) {
        Part::Bytes(bytes) => NodeOutput::Str(String::from_utf8_lossy(bytes).into_owned()),
        Part::Absent(reason) => return Err(format!("tree path `{path}` absent: {reason}")),
    };
    driver.borrow_mut().seed(node, value);
    Ok(())
}

fn materialize_capability_request(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    node: NodeId,
    request: &CapabilityRequestMeta,
) -> Result<(), String> {
    let providers: Vec<Value> = runtime.providers.providers(&request.spec).to_vec();
    for provider in providers {
        if capability_satisfied(&request.constraint, &provider)? {
            let output = {
                let mut driver = driver.borrow_mut();
                node_output_from_value_in_driver(
                    &mut driver,
                    Rc::clone(&externals),
                    runtime.clone(),
                    node,
                    provider,
                )?
            };
            driver.borrow_mut().seed(node, output);
            return Ok(());
        }
    }
    Err(format!(
        "no advertised provider of `{}` satisfies the requested constraint",
        request.spec
    ))
}

fn capability_satisfied(constraint: &Constraint, provider: &Value) -> Result<bool, String> {
    match constraint {
        Constraint::Always => Ok(true),
        Constraint::All(children) => {
            for child in children {
                if !capability_satisfied(child, provider)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Constraint::Any(children) => {
            for child in children {
                if capability_satisfied(child, provider)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Constraint::Cmp { field, op, value } => {
            let attr = provider_attr(provider, field)?;
            Ok(capability_attr_satisfies(&attr, *op, value))
        }
    }
}

fn provider_attr(provider: &Value, field: &str) -> Result<Value, String> {
    let Value::Struct(fields) = provider else {
        return Err(format!(
            "capability provider must be a struct, got {provider:?}"
        ));
    };
    let thunk = fields
        .iter()
        .find(|(name, _)| name == field)
        .map(|(_, thunk)| thunk)
        .ok_or_else(|| format!("capability provider has no `{field}` field"))?;
    thunk
        .materialize_for_graph_boundary()
        .map_err(|poison| poison.message())
}

fn capability_attr_satisfies(attr: &Value, op: CmpOp, value: &AttrLit) -> bool {
    let ordering = match (attr, value) {
        (Value::U64(actual), AttrLit::Int(want)) => actual.cmp(want),
        (Value::Str(actual), AttrLit::Str(want)) => actual.as_ref().cmp(want.as_str()),
        (Value::Bool(actual), AttrLit::Bool(want)) => actual.cmp(want),
        _ => return false,
    };
    op.holds(ordering)
}

fn materialize_host_method(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    method: HostMethod,
) -> Result<(), String> {
    let node = boundary.node();
    let inputs = {
        let driver = driver.borrow();
        driver
            .node(node)
            .ok_or_else(|| format!("graph host boundary {node:?} has no node"))?
            .inputs
            .clone()
    };

    match method {
        HostMethod::Which => materialize_host_which(driver, externals, runtime, boundary, inputs),
        HostMethod::Exec => materialize_host_exec(driver, externals, runtime, boundary, inputs),
    }
}

fn materialize_host_which(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    inputs: Vec<NodeId>,
) -> Result<(), String> {
    let node = boundary.node();
    let [receiver, name] = inputs.as_slice() else {
        return Err(format!(
            "host.which graph node expects receiver and name, got {} inputs",
            inputs.len()
        ));
    };
    ensure_host_receiver(demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        *receiver,
    )?)?;
    let name = expect_string(
        demand_payload(
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime,
            boundary,
            *name,
        )?,
        "host.which name",
    )?;
    let path = crate::host::which(&name).ok_or_else(|| format!("host tool `{name}` not found"))?;
    driver.borrow_mut().seed(node, NodeOutput::Str(path));
    Ok(())
}

fn materialize_host_exec(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    inputs: Vec<NodeId>,
) -> Result<(), String> {
    let node = boundary.node();
    let [receiver, argv] = inputs.as_slice() else {
        return Err(format!(
            "host.exec graph node expects receiver and argv array, got {} inputs",
            inputs.len()
        ));
    };
    ensure_host_receiver(demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        *receiver,
    )?)?;
    let argv = demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        runtime.clone(),
        boundary,
        *argv,
    )?;
    let fields = expect_array_fields(argv, "host.exec argv")?;
    let mut args = Vec::with_capacity(fields.len());
    for field in fields {
        let value = demand_payload(
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
            boundary,
            field,
        )?;
        args.push(expect_string(value, "host.exec argv element")?);
    }

    let cache_key = host_exec_key(&args)?;
    if let Some(cached) = runtime.cache.get_host_exec(&cache_key) {
        let output = {
            let mut driver = driver.borrow_mut();
            host_exec_output(&mut driver, cached)
        };
        driver.borrow_mut().seed(node, output);
        return Ok(());
    }

    let observed = crate::host::exec_with_fingerprint(&args, cache_key.program_fingerprint.clone())
        .map_err(|e| format!("host.exec: {e}"))?;
    runtime.cache.put_host_exec(cache_key, observed.clone());
    let output = {
        let mut driver = driver.borrow_mut();
        host_exec_output(&mut driver, observed)
    };
    driver.borrow_mut().seed(node, output);
    Ok(())
}

fn host_exec_key(args: &[String]) -> Result<HostExecCacheKey, String> {
    let program = args
        .first()
        .ok_or_else(|| "host.exec needs a program".to_string())?;
    Ok(HostExecCacheKey {
        argv: args.to_vec(),
        program_fingerprint: crate::host::fingerprint_executable(program)?,
    })
}

fn demand_payload(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    boundary: &BoundaryDemand,
    node: NodeId,
) -> Result<NodeOutput, String> {
    match drive_request(driver, externals, runtime, boundary.payload(node))? {
        Ok(resolved) => Ok(resolved.into_output()),
        Err(error) => Err(driver_error_message(error)),
    }
}

fn ensure_host_receiver(output: NodeOutput) -> Result<(), String> {
    match output {
        NodeOutput::Struct { .. } => Ok(()),
        other => Err(format!(
            "host method receiver is not a Host struct: {other:?}"
        )),
    }
}

fn expect_string(output: NodeOutput, context: &str) -> Result<String, String> {
    match output {
        NodeOutput::Str(value) => Ok(value),
        other => Err(format!("{context} must be a String, got {other:?}")),
    }
}

fn expect_array_fields(output: NodeOutput, context: &str) -> Result<Vec<NodeId>, String> {
    match output {
        NodeOutput::LazyData { label, fields } if label.starts_with('[') => Ok(fields),
        other => Err(format!("{context} must be an Array, got {other:?}")),
    }
}

fn host_exec_output(driver: &mut Driver, output: crate::host::HostExec) -> NodeOutput {
    let field_names = ["exit_code", "stdout", "stderr", "fingerprint"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let fields = vec![
        push_const_node(
            driver,
            "host.exec.exit_code",
            ConstValue::U64(output.exit_code as u64),
        ),
        push_const_node(driver, "host.exec.stdout", ConstValue::Str(output.stdout)),
        push_const_node(driver, "host.exec.stderr", ConstValue::Str(output.stderr)),
        push_const_node(
            driver,
            "host.exec.fingerprint",
            ConstValue::Str(output.fingerprint),
        ),
    ];
    NodeOutput::Struct {
        field_names,
        fields,
    }
}

fn push_const_node(driver: &mut Driver, label: &str, value: ConstValue) -> NodeId {
    driver.push_node(NodeKind::Const(value), label.to_string(), Vec::new())
}

fn seed_pattern(
    program: &VirProgram,
    pattern: PatternId,
    value: Value,
    binding_nodes: &HashMap<vx_ir::ids::BindingId, NodeId>,
    graph: &mut vx_runtime_graph::DemandGraph,
    externals: &mut HashMap<NodeId, Thunk>,
    runtime: &RuntimeBoundaries,
    seeds: &mut Vec<(NodeId, NodeOutput)>,
) -> Result<(), String> {
    match &program.arena[pattern].kind {
        PatternKind::Binding(binding) => {
            let node = binding_nodes
                .get(&binding.binding)
                .copied()
                .ok_or_else(|| format!("graph body has no node for binding `{}`", binding.name))?;
            seeds.push((
                node,
                node_output_from_value(graph, externals, runtime, node, value)?,
            ));
            Ok(())
        }
        PatternKind::At(at) => {
            let node = binding_nodes
                .get(&at.binding)
                .copied()
                .ok_or_else(|| format!("graph body has no node for binding `{}`", at.name))?;
            seeds.push((
                node,
                node_output_from_value(graph, externals, runtime, node, value.clone())?,
            ));
            seed_pattern(
                program,
                at.inner,
                value,
                binding_nodes,
                graph,
                externals,
                runtime,
                seeds,
            )
        }
        PatternKind::Wildcard => Ok(()),
        other => Err(format!(
            "graph body parameter pattern is not seedable yet: {other:?}"
        )),
    }
}

fn node_output_from_value(
    graph: &mut vx_runtime_graph::DemandGraph,
    externals: &mut HashMap<NodeId, Thunk>,
    runtime: &RuntimeBoundaries,
    node: NodeId,
    value: Value,
) -> Result<NodeOutput, String> {
    match value {
        Value::U64(n) => Ok(NodeOutput::U64(n)),
        Value::Bool(b) => Ok(NodeOutput::Bool(b)),
        Value::Str(s) => Ok(NodeOutput::Str(s.to_string())),
        Value::Unit => Ok(NodeOutput::Unit),
        Value::Tree(handle) => {
            runtime.remote_trees.borrow_mut().insert(node, handle);
            Ok(NodeOutput::OutputTree)
        }
        Value::OutputTree => Ok(NodeOutput::OutputTree),
        Value::Tuple(fields) => Ok(NodeOutput::LazyData {
            label: "(external)".to_string(),
            fields: external_field_nodes(
                graph,
                externals,
                fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, thunk)| (index.to_string(), thunk)),
            ),
        }),
        Value::Array(fields) => Ok(NodeOutput::LazyData {
            label: "[external]".to_string(),
            fields: external_field_nodes(
                graph,
                externals,
                fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, thunk)| (index.to_string(), thunk)),
            ),
        }),
        Value::Struct(fields) => {
            let field_names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
            let fields = external_field_nodes(graph, externals, fields.into_iter());
            Ok(NodeOutput::Struct {
                field_names,
                fields,
            })
        }
        Value::Variant { tag, fields } => {
            let field_names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
            let fields = external_field_nodes(graph, externals, fields.into_iter());
            Ok(NodeOutput::Variant {
                tag,
                field_names,
                fields,
            })
        }
        other => Err(format!(
            "graph body argument value is not seedable yet: {other:?}"
        )),
    }
}

fn node_output_from_value_in_driver(
    driver: &mut Driver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    node: NodeId,
    value: Value,
) -> Result<NodeOutput, String> {
    match value {
        Value::U64(n) => Ok(NodeOutput::U64(n)),
        Value::Bool(b) => Ok(NodeOutput::Bool(b)),
        Value::Str(s) => Ok(NodeOutput::Str(s.to_string())),
        Value::Unit => Ok(NodeOutput::Unit),
        Value::Tree(handle) => {
            runtime.remote_trees.borrow_mut().insert(node, handle);
            Ok(NodeOutput::OutputTree)
        }
        Value::OutputTree => Ok(NodeOutput::OutputTree),
        Value::Tuple(fields) => Ok(NodeOutput::LazyData {
            label: "(external)".to_string(),
            fields: external_field_nodes_in_driver(
                driver,
                externals,
                fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, thunk)| (index.to_string(), thunk)),
            ),
        }),
        Value::Array(fields) => Ok(NodeOutput::LazyData {
            label: "[external]".to_string(),
            fields: external_field_nodes_in_driver(
                driver,
                externals,
                fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, thunk)| (index.to_string(), thunk)),
            ),
        }),
        Value::Struct(fields) => {
            let field_names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
            let fields = external_field_nodes_in_driver(driver, externals, fields.into_iter());
            Ok(NodeOutput::Struct {
                field_names,
                fields,
            })
        }
        Value::Variant { tag, fields } => {
            let field_names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
            let fields = external_field_nodes_in_driver(driver, externals, fields.into_iter());
            Ok(NodeOutput::Variant {
                tag,
                field_names,
                fields,
            })
        }
        other => Err(format!(
            "graph boundary value is not materializable yet: {other:?}"
        )),
    }
}

fn external_field_nodes(
    graph: &mut vx_runtime_graph::DemandGraph,
    externals: &mut HashMap<NodeId, Thunk>,
    fields: impl IntoIterator<Item = (String, Thunk)>,
) -> Vec<NodeId> {
    fields
        .into_iter()
        .map(|(name, thunk)| {
            let node = NodeId(graph.nodes.len() as u32);
            graph.nodes.push(vx_runtime_graph::Node {
                id: node,
                kind: NodeKind::Boundary,
                span: None,
                label: format!("external.{name}"),
                ty: "?".to_string(),
                inputs: Vec::new(),
            });
            externals.insert(node, thunk);
            node
        })
        .collect()
}

fn external_field_nodes_in_driver(
    driver: &mut Driver,
    externals: ExternalThunks,
    fields: impl IntoIterator<Item = (String, Thunk)>,
) -> Vec<NodeId> {
    fields
        .into_iter()
        .map(|(name, thunk)| {
            let node = driver.push_node(NodeKind::Boundary, format!("external.{name}"), Vec::new());
            externals.borrow_mut().insert(node, thunk);
            node
        })
        .collect()
}

fn value_from_resolved(
    resolved: ResolvedNode,
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
) -> Result<Value, String> {
    let output = resolved.output().clone();
    match output {
        NodeOutput::U64(n) => Ok(Value::U64(n)),
        NodeOutput::Bool(b) => Ok(Value::Bool(b)),
        NodeOutput::Str(s) => Ok(Value::Str(Rc::from(s))),
        NodeOutput::Unit => Ok(Value::Unit),
        NodeOutput::OutputTree => Ok(Value::OutputTree),
        NodeOutput::LazyData { label, fields } => {
            let fields = fields
                .into_iter()
                .map(|node| {
                    let demand = resolved.field_demand(node).ok_or_else(|| {
                        format!(
                            "node {node:?} is not a public field of {:?}",
                            resolved.node()
                        )
                    })?;
                    Ok(graph_field_thunk(
                        Rc::clone(&driver),
                        Rc::clone(&externals),
                        runtime.clone(),
                        demand,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            if label.starts_with('[') {
                Ok(Value::Array(fields))
            } else {
                Ok(Value::Tuple(fields))
            }
        }
        NodeOutput::Struct {
            field_names,
            fields,
        } => {
            let fields = field_names
                .into_iter()
                .zip(fields)
                .map(|(name, node)| {
                    let demand = resolved.field_demand(node).ok_or_else(|| {
                        format!(
                            "node {node:?} is not a public field of {:?}",
                            resolved.node()
                        )
                    })?;
                    Ok((
                        name,
                        graph_field_thunk(
                            Rc::clone(&driver),
                            Rc::clone(&externals),
                            runtime.clone(),
                            demand,
                        ),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Value::Struct(fields))
        }
        NodeOutput::Variant {
            tag,
            field_names,
            fields,
        } => {
            let fields = field_names
                .into_iter()
                .zip(fields)
                .map(|(name, node)| {
                    let demand = resolved.field_demand(node).ok_or_else(|| {
                        format!(
                            "node {node:?} is not a public field of {:?}",
                            resolved.node()
                        )
                    })?;
                    Ok((
                        name,
                        graph_field_thunk(
                            Rc::clone(&driver),
                            Rc::clone(&externals),
                            runtime.clone(),
                            demand,
                        ),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Value::Variant { tag, fields })
        }
        NodeOutput::Failure { err, context } => Ok(Value::Struct(vec![
            (
                "err".to_string(),
                ready_output_thunk(
                    *err,
                    Rc::clone(&driver),
                    Rc::clone(&externals),
                    runtime.clone(),
                ),
            ),
            (
                "context".to_string(),
                ready_output_thunk(
                    NodeOutput::FailureContext(context),
                    Rc::clone(&driver),
                    Rc::clone(&externals),
                    runtime.clone(),
                ),
            ),
        ])),
        NodeOutput::FailureContext(context) => Ok(Value::FailureContext(Rc::new(
            failure_context_from_core(context),
        ))),
    }
}

fn value_from_ready_output(
    output: NodeOutput,
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
) -> Result<Value, String> {
    match output {
        NodeOutput::LazyData { .. } | NodeOutput::Struct { .. } | NodeOutput::Variant { .. } => {
            Err("compound ready output has no public field demand authority".to_string())
        }
        output => value_from_scalar_output(output, driver, externals, runtime),
    }
}

fn value_from_scalar_output(
    output: NodeOutput,
    _driver: SharedDriver,
    _externals: ExternalThunks,
    _runtime: RuntimeBoundaries,
) -> Result<Value, String> {
    match output {
        NodeOutput::U64(n) => Ok(Value::U64(n)),
        NodeOutput::Bool(b) => Ok(Value::Bool(b)),
        NodeOutput::Str(s) => Ok(Value::Str(Rc::from(s))),
        NodeOutput::Unit => Ok(Value::Unit),
        NodeOutput::OutputTree => Ok(Value::OutputTree),
        NodeOutput::FailureContext(context) => Ok(Value::FailureContext(Rc::new(
            failure_context_from_core(context),
        ))),
        NodeOutput::Failure { err, context } => Ok(Value::Struct(vec![
            (
                "err".to_string(),
                ready_output_thunk(*err, _driver.clone(), _externals.clone(), _runtime.clone()),
            ),
            (
                "context".to_string(),
                ready_output_thunk(
                    NodeOutput::FailureContext(context),
                    _driver,
                    _externals,
                    _runtime,
                ),
            ),
        ])),
        NodeOutput::LazyData { .. } | NodeOutput::Struct { .. } | NodeOutput::Variant { .. } => {
            Err("compound output needs public field demand authority".to_string())
        }
    }
}

fn graph_field_thunk(
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
    demand: PublicValueDemand,
) -> Thunk {
    Thunk::native(move || {
        let result = drive_request(
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
            demand.request(),
        )
        .map_err(poison_from_string)?;
        let resolved = result.map_err(poison_from_driver_error)?;
        value_from_resolved(
            resolved,
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
        )
        .map_err(poison_from_string)
    })
}

fn ready_output_thunk(
    output: NodeOutput,
    driver: SharedDriver,
    externals: ExternalThunks,
    runtime: RuntimeBoundaries,
) -> Thunk {
    Thunk::native(move || {
        value_from_ready_output(
            output.clone(),
            Rc::clone(&driver),
            Rc::clone(&externals),
            runtime.clone(),
        )
        .map_err(poison_from_string)
    })
}

fn poison_from_driver_error(error: DriverError) -> Poison {
    match error {
        DriverError::Poison(poison) => poison_from_core(poison),
        DriverError::Unsupported(unsupported) => poison_from_unsupported(unsupported),
        DriverError::Boundary(boundary) => {
            let node = boundary.node();
            poison_from_string(format!(
                "graph boundary at node {node:?}: boundary dispatch is not wired yet"
            ))
        }
        DriverError::Stalled(node) => poison_from_string(format!("graph stalled at node {node:?}")),
    }
}

fn driver_error_message(error: DriverError) -> String {
    match error {
        DriverError::Poison(poison) => poison.reason,
        DriverError::Unsupported(unsupported) => format!(
            "graph unsupported at node {:?}: {}",
            unsupported.node, unsupported.reason
        ),
        DriverError::Boundary(boundary) => {
            let node = boundary.node();
            format!("graph boundary at node {node:?}: boundary dispatch is not wired yet")
        }
        DriverError::Stalled(node) => format!("graph stalled at node {node:?}"),
    }
}

fn poison_from_core(poison: CorePoison) -> Poison {
    Poison {
        err: poison_err_from_output(*poison.err)
            .unwrap_or_else(|| Value::Str(Rc::from(poison.reason))),
        context: failure_context_from_core(poison.context),
    }
}

fn failure_context_from_core(context: CoreFailureContext) -> FailureContext {
    FailureContext {
        span: context.span,
        demand_path: context.demand_path,
    }
}

fn poison_err_from_output(output: NodeOutput) -> Option<Value> {
    match output {
        NodeOutput::U64(n) => Some(Value::U64(n)),
        NodeOutput::Bool(b) => Some(Value::Bool(b)),
        NodeOutput::Str(s) => Some(Value::Str(Rc::from(s))),
        NodeOutput::Unit => Some(Value::Unit),
        NodeOutput::OutputTree => Some(Value::OutputTree),
        NodeOutput::Variant {
            tag,
            field_names,
            fields,
        } if field_names.is_empty() && fields.is_empty() => Some(Value::Variant {
            tag,
            fields: Vec::new(),
        }),
        NodeOutput::Failure { err, .. } => poison_err_from_output(*err),
        NodeOutput::FailureContext(context) => Some(Value::FailureContext(Rc::new(
            failure_context_from_core(context),
        ))),
        NodeOutput::LazyData { .. } | NodeOutput::Struct { .. } | NodeOutput::Variant { .. } => {
            None
        }
    }
}

fn poison_from_unsupported(unsupported: UnsupportedNode) -> Poison {
    poison_from_string(format!(
        "graph unsupported at node {:?}: {}",
        unsupported.node, unsupported.reason
    ))
}

fn poison_from_string(reason: String) -> Poison {
    Poison {
        err: Value::Str(Rc::from(reason)),
        context: FailureContext::default(),
    }
}
