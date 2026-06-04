use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vx_ir::build::VirProgram;
use vx_ir::ids::{BodyId, PatternId};
use vx_ir::pattern::PatternKind;
use vx_runtime_core::{CoreFailureContext, CorePoison, NodeOutput, UnsupportedNode};
use vx_runtime_driver::{DemandRequest, Driver, DriverError};
use vx_runtime_graph::{ConstValue, HostMethod, NodeId, NodeKind};
use vx_types::TypeEnv;

use crate::eval::{FailureContext, Outcome, Poison, Thunk, Value};

type SharedDriver = Rc<RefCell<Driver>>;
type ExternalThunks = Rc<RefCell<HashMap<NodeId, Thunk>>>;

pub(crate) fn run_source(source: &str, entry: &str) -> Result<(Outcome, u64), String> {
    let graph = crate::analyze::analyze_source(source, entry)?;
    drive_graph(graph, Vec::new(), HashMap::new())
}

pub(crate) fn run_body(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
) -> Result<(Outcome, u64), String> {
    let (mut graph, _, _, binding_nodes) = crate::analyze::build_graph_indexed(program, env, body);
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
            &mut seeds,
        )?;
    }
    drive_graph(graph, seeds, externals)
}

fn drive_graph(
    graph: vx_runtime_graph::DemandGraph,
    seeds: Vec<(NodeId, NodeOutput)>,
    externals: HashMap<NodeId, Thunk>,
) -> Result<(Outcome, u64), String> {
    let root = graph.root;
    let mut driver = Driver::new(graph);
    for (node, output) in seeds {
        driver = driver.with_seed(node, output);
    }
    let driver = Rc::new(RefCell::new(driver));
    let externals = Rc::new(RefCell::new(externals));
    let result = drive_request(
        Rc::clone(&driver),
        Rc::clone(&externals),
        DemandRequest::root(root),
    )?;
    let kernel_executions = driver.borrow().kernel_executions();
    let outcome = match result {
        Ok(output) => Outcome::Clean(value_from_output(
            output,
            Rc::clone(&driver),
            Rc::clone(&externals),
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
        Err(DriverError::Boundary(node)) => {
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
    request: DemandRequest,
) -> Result<Result<NodeOutput, DriverError>, String> {
    loop {
        let result = {
            let mut driver = driver.borrow_mut();
            driver.drive(request)
        };
        match result {
            Err(DriverError::Boundary(node)) => {
                if materialize_boundary(Rc::clone(&driver), Rc::clone(&externals), node)? {
                    continue;
                }
                return Ok(Err(DriverError::Boundary(node)));
            }
            other => return Ok(other),
        }
    }
}

fn materialize_boundary(
    driver: SharedDriver,
    externals: ExternalThunks,
    node: NodeId,
) -> Result<bool, String> {
    if externals.borrow().contains_key(&node) {
        materialize_external_boundary(driver, externals, node)?;
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

    materialize_host_method(driver, externals, node, method)?;
    Ok(true)
}

fn materialize_external_boundary(
    driver: SharedDriver,
    externals: ExternalThunks,
    node: NodeId,
) -> Result<(), String> {
    let thunk = externals
        .borrow_mut()
        .remove(&node)
        .ok_or_else(|| format!("graph external boundary {node:?} has no thunk"))?;
    let value = thunk
        .force_for_graph_boundary()
        .map_err(|poison| poison.message())?;
    let output = {
        let mut driver = driver.borrow_mut();
        node_output_from_value_in_driver(&mut driver, Rc::clone(&externals), value)?
    };
    driver.borrow_mut().seed(node, output);
    Ok(())
}

fn materialize_host_method(
    driver: SharedDriver,
    externals: ExternalThunks,
    node: NodeId,
    method: HostMethod,
) -> Result<(), String> {
    let inputs = {
        let driver = driver.borrow();
        driver
            .node(node)
            .ok_or_else(|| format!("graph host boundary {node:?} has no node"))?
            .inputs
            .clone()
    };

    match method {
        HostMethod::Which => materialize_host_which(driver, externals, node, inputs),
        HostMethod::Exec => materialize_host_exec(driver, externals, node, inputs),
    }
}

fn materialize_host_which(
    driver: SharedDriver,
    externals: ExternalThunks,
    node: NodeId,
    inputs: Vec<NodeId>,
) -> Result<(), String> {
    let [receiver, name] = inputs.as_slice() else {
        return Err(format!(
            "host.which graph node expects receiver and name, got {} inputs",
            inputs.len()
        ));
    };
    ensure_host_receiver(demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        *receiver,
    )?)?;
    let name = expect_string(
        demand_payload(Rc::clone(&driver), Rc::clone(&externals), *name)?,
        "host.which name",
    )?;
    let path = crate::host::which(&name).ok_or_else(|| format!("host tool `{name}` not found"))?;
    driver.borrow_mut().seed(node, NodeOutput::Str(path));
    Ok(())
}

fn materialize_host_exec(
    driver: SharedDriver,
    externals: ExternalThunks,
    node: NodeId,
    inputs: Vec<NodeId>,
) -> Result<(), String> {
    let [receiver, argv] = inputs.as_slice() else {
        return Err(format!(
            "host.exec graph node expects receiver and argv array, got {} inputs",
            inputs.len()
        ));
    };
    ensure_host_receiver(demand_payload(
        Rc::clone(&driver),
        Rc::clone(&externals),
        *receiver,
    )?)?;
    let argv = demand_payload(Rc::clone(&driver), Rc::clone(&externals), *argv)?;
    let fields = expect_array_fields(argv, "host.exec argv")?;
    let mut args = Vec::with_capacity(fields.len());
    for field in fields {
        let value = demand_payload(Rc::clone(&driver), Rc::clone(&externals), field)?;
        args.push(expect_string(value, "host.exec argv element")?);
    }

    let observed = crate::host::exec(&args).map_err(|e| format!("host.exec: {e}"))?;
    let output = {
        let mut driver = driver.borrow_mut();
        host_exec_output(&mut driver, observed)
    };
    driver.borrow_mut().seed(node, output);
    Ok(())
}

fn demand_payload(
    driver: SharedDriver,
    externals: ExternalThunks,
    node: NodeId,
) -> Result<NodeOutput, String> {
    match drive_request(driver, externals, DemandRequest::boundary_payload(node))? {
        Ok(output) => Ok(output),
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
    seeds: &mut Vec<(NodeId, NodeOutput)>,
) -> Result<(), String> {
    match &program.arena[pattern].kind {
        PatternKind::Binding(binding) => {
            let node = binding_nodes
                .get(&binding.binding)
                .copied()
                .ok_or_else(|| format!("graph body has no node for binding `{}`", binding.name))?;
            seeds.push((node, node_output_from_value(graph, externals, value)?));
            Ok(())
        }
        PatternKind::At(at) => {
            let node = binding_nodes
                .get(&at.binding)
                .copied()
                .ok_or_else(|| format!("graph body has no node for binding `{}`", at.name))?;
            seeds.push((
                node,
                node_output_from_value(graph, externals, value.clone())?,
            ));
            seed_pattern(
                program,
                at.inner,
                value,
                binding_nodes,
                graph,
                externals,
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
    value: Value,
) -> Result<NodeOutput, String> {
    match value {
        Value::U64(n) => Ok(NodeOutput::U64(n)),
        Value::Bool(b) => Ok(NodeOutput::Bool(b)),
        Value::Str(s) => Ok(NodeOutput::Str(s.to_string())),
        Value::Unit => Ok(NodeOutput::Unit),
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
    value: Value,
) -> Result<NodeOutput, String> {
    match value {
        Value::U64(n) => Ok(NodeOutput::U64(n)),
        Value::Bool(b) => Ok(NodeOutput::Bool(b)),
        Value::Str(s) => Ok(NodeOutput::Str(s.to_string())),
        Value::Unit => Ok(NodeOutput::Unit),
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

fn value_from_output(
    output: NodeOutput,
    driver: SharedDriver,
    externals: ExternalThunks,
) -> Result<Value, String> {
    match output {
        NodeOutput::U64(n) => Ok(Value::U64(n)),
        NodeOutput::Bool(b) => Ok(Value::Bool(b)),
        NodeOutput::Str(s) => Ok(Value::Str(Rc::from(s))),
        NodeOutput::Unit => Ok(Value::Unit),
        NodeOutput::OutputTree => Ok(Value::OutputTree),
        NodeOutput::LazyData { label, fields } => {
            let fields = fields
                .into_iter()
                .map(|node| graph_field_thunk(Rc::clone(&driver), Rc::clone(&externals), node))
                .collect();
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
                    (
                        name,
                        graph_field_thunk(Rc::clone(&driver), Rc::clone(&externals), node),
                    )
                })
                .collect();
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
                    (
                        name,
                        graph_field_thunk(Rc::clone(&driver), Rc::clone(&externals), node),
                    )
                })
                .collect();
            Ok(Value::Variant { tag, fields })
        }
        NodeOutput::Failure { err, context } => Ok(Value::Struct(vec![
            (
                "err".to_string(),
                ready_output_thunk(*err, Rc::clone(&driver), Rc::clone(&externals)),
            ),
            (
                "context".to_string(),
                ready_output_thunk(
                    NodeOutput::FailureContext(context),
                    Rc::clone(&driver),
                    Rc::clone(&externals),
                ),
            ),
        ])),
        NodeOutput::FailureContext(context) => Ok(Value::FailureContext(Rc::new(
            failure_context_from_core(context),
        ))),
    }
}

fn graph_field_thunk(driver: SharedDriver, externals: ExternalThunks, node: NodeId) -> Thunk {
    Thunk::native(move || {
        let result = drive_request(
            Rc::clone(&driver),
            Rc::clone(&externals),
            DemandRequest::public_value_field(node),
        )
        .map_err(poison_from_string)?;
        let output = result.map_err(poison_from_driver_error)?;
        value_from_output(output, Rc::clone(&driver), Rc::clone(&externals))
            .map_err(poison_from_string)
    })
}

fn ready_output_thunk(
    output: NodeOutput,
    driver: SharedDriver,
    externals: ExternalThunks,
) -> Thunk {
    Thunk::native(move || {
        value_from_output(output.clone(), Rc::clone(&driver), Rc::clone(&externals))
            .map_err(poison_from_string)
    })
}

fn poison_from_driver_error(error: DriverError) -> Poison {
    match error {
        DriverError::Poison(poison) => poison_from_core(poison),
        DriverError::Unsupported(unsupported) => poison_from_unsupported(unsupported),
        DriverError::Boundary(node) => poison_from_string(format!(
            "graph boundary at node {node:?}: boundary dispatch is not wired yet"
        )),
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
        DriverError::Boundary(node) => {
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
