use std::cell::RefCell;
use std::rc::Rc;

use vx_runtime_core::{CoreFailureContext, CorePoison, NodeOutput, UnsupportedNode};
use vx_runtime_driver::{DemandRequest, Driver, DriverError};
use vx_runtime_graph::NodeId;

use crate::eval::{FailureContext, Outcome, Poison, Thunk, Value};

type SharedDriver = Rc<RefCell<Driver>>;

pub(crate) fn run_source(source: &str, entry: &str) -> Result<(Outcome, u64), String> {
    let graph = crate::analyze::analyze_source(source, entry)?;
    let root = graph.root;
    let driver = Rc::new(RefCell::new(Driver::new(graph)));
    let result = {
        let mut driver = driver.borrow_mut();
        driver.drive(DemandRequest::root(root))
    };
    let kernel_executions = driver.borrow().kernel_executions();
    let outcome = match result {
        Ok(output) => Outcome::Clean(value_from_output(output, Rc::clone(&driver))?),
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

fn value_from_output(output: NodeOutput, driver: SharedDriver) -> Result<Value, String> {
    match output {
        NodeOutput::U64(n) => Ok(Value::U64(n)),
        NodeOutput::Bool(b) => Ok(Value::Bool(b)),
        NodeOutput::Str(s) => Ok(Value::Str(Rc::from(s))),
        NodeOutput::Unit => Ok(Value::Unit),
        NodeOutput::LazyData { label, fields } => {
            let fields = fields
                .into_iter()
                .map(|node| graph_field_thunk(Rc::clone(&driver), node))
                .collect();
            if label.starts_with('[') {
                Ok(Value::Array(fields))
            } else {
                Ok(Value::Tuple(fields))
            }
        }
        NodeOutput::Variant {
            tag,
            field_names,
            fields,
        } => {
            let fields = field_names
                .into_iter()
                .zip(fields)
                .map(|(name, node)| (name, graph_field_thunk(Rc::clone(&driver), node)))
                .collect();
            Ok(Value::Variant { tag, fields })
        }
        NodeOutput::Failure { err, context } => Ok(Value::Struct(vec![
            (
                "err".to_string(),
                ready_output_thunk(*err, Rc::clone(&driver)),
            ),
            (
                "context".to_string(),
                ready_output_thunk(NodeOutput::FailureContext(context), Rc::clone(&driver)),
            ),
        ])),
        NodeOutput::FailureContext(context) => Ok(Value::FailureContext(Rc::new(
            failure_context_from_core(context),
        ))),
    }
}

fn graph_field_thunk(driver: SharedDriver, node: NodeId) -> Thunk {
    Thunk::native(move || {
        let result = {
            let mut driver = driver.borrow_mut();
            driver.drive(DemandRequest::public_value_field(node))
        };
        let output = result.map_err(poison_from_driver_error)?;
        value_from_output(output, Rc::clone(&driver)).map_err(poison_from_string)
    })
}

fn ready_output_thunk(output: NodeOutput, driver: SharedDriver) -> Thunk {
    Thunk::native(move || {
        value_from_output(output.clone(), Rc::clone(&driver)).map_err(poison_from_string)
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
        NodeOutput::LazyData { .. } | NodeOutput::Variant { .. } => None,
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
