use std::rc::Rc;

use vx_runtime_core::NodeOutput;
use vx_runtime_driver::{DemandRequest, Driver, DriverError};

use crate::eval::{Outcome, Value};

pub(crate) fn run_source(source: &str, entry: &str) -> Result<(Outcome, u64), String> {
    let graph = crate::analyze::analyze_source(source, entry)?;
    let root = graph.root;
    let mut driver = Driver::new(graph);
    let outcome = match driver.drive(DemandRequest::root(root)) {
        Ok(output) => Outcome::Clean(value_from_output(output)?),
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
    Ok((outcome, 0))
}

fn value_from_output(output: NodeOutput) -> Result<Value, String> {
    match output {
        NodeOutput::U64(n) => Ok(Value::U64(n)),
        NodeOutput::Bool(b) => Ok(Value::Bool(b)),
        NodeOutput::Str(s) => Ok(Value::Str(Rc::from(s))),
        NodeOutput::Unit => Ok(Value::Unit),
        NodeOutput::LazyData { label, .. } => Err(format!(
            "graph root resolved to lazy data `{label}`; node-reference values are not wired to public Value yet"
        )),
    }
}
