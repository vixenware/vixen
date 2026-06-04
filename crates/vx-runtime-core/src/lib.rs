//! Graph stepper for Vixen runtime nodes.
//!
//! This crate is intentionally source-blind. It can inspect graph nodes and
//! report missing dependencies, but it does not own scheduling, memo storage, or
//! external demand origins.

use std::collections::HashMap;

use vx_runtime_graph::{BinKind, ConstValue, DemandGraph, KernelOp, NodeId, NodeKind, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOutput {
    U64(u64),
    Bool(bool),
    Str(String),
    Unit,
    LazyData { label: String, fields: Vec<NodeId> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorePoison {
    pub node: NodeId,
    pub reason: String,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedNode {
    pub node: NodeId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryRequest {
    pub node: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedSet {
    Nodes(Vec<NodeId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Need(NeedSet),
    Ready(NodeOutput),
    Boundary(BoundaryRequest),
    Poison(CorePoison),
    Unsupported(UnsupportedNode),
}

pub struct ReadyInputs<'a> {
    values: &'a HashMap<NodeId, NodeOutput>,
}

impl<'a> ReadyInputs<'a> {
    pub fn new(values: &'a HashMap<NodeId, NodeOutput>) -> Self {
        Self { values }
    }

    pub fn get(&self, node: NodeId) -> Option<&'a NodeOutput> {
        self.values.get(&node)
    }
}

pub fn step(graph: &DemandGraph, node: NodeId, inputs: ReadyInputs<'_>) -> Step {
    let Some(n) = graph.nodes.get(node.0 as usize) else {
        return unsupported(node, "node index out of bounds");
    };
    match &n.kind {
        NodeKind::Const(value) => match const_output(value) {
            Some(output) => Step::Ready(output),
            None => unsupported(node, format!("unsupported const `{value:?}`")),
        },
        NodeKind::Param => unsupported(node, "parameter has no supplied value"),
        NodeKind::LazyData => Step::Ready(NodeOutput::LazyData {
            label: n.label.clone(),
            fields: n.inputs.clone(),
        }),
        NodeKind::Project { index } => project(graph, node, n.inputs.as_slice(), *index, inputs),
        NodeKind::Kernel(op) => kernel(graph, node, n.inputs.as_slice(), *op, inputs),
        NodeKind::Boundary => Step::Boundary(BoundaryRequest { node }),
        NodeKind::Bomb => poisoned(graph, node, "bomb demanded"),
        NodeKind::Fail => fail(graph, node, n.inputs.as_slice(), inputs),
        NodeKind::Unresolved => unsupported(node, format!("unresolved node `{}`", n.label)),
    }
}

fn project(
    graph: &DemandGraph,
    node: NodeId,
    raw_inputs: &[NodeId],
    index: u32,
    inputs: ReadyInputs<'_>,
) -> Step {
    if let Some(target) = graph.resolved_value(node)
        && target != node
    {
        return match inputs.get(target) {
            Some(output) => Step::Ready(output.clone()),
            None => Step::Need(NeedSet::Nodes(vec![target])),
        };
    }

    let Some(&container) = raw_inputs.first() else {
        return unsupported(node, "project node has no container");
    };
    let Some(container_output) = inputs.get(container) else {
        return Step::Need(NeedSet::Nodes(vec![container]));
    };
    let NodeOutput::LazyData { fields, .. } = container_output else {
        return unsupported(node, "project container is not lazy data");
    };
    let Some(&field) = fields.get(index as usize) else {
        return unsupported(node, "project index out of bounds");
    };
    match inputs.get(field) {
        Some(output) => Step::Ready(output.clone()),
        None => Step::Need(NeedSet::Nodes(vec![field])),
    }
}

fn kernel(
    graph: &DemandGraph,
    node: NodeId,
    raw_inputs: &[NodeId],
    op: KernelOp,
    inputs: ReadyInputs<'_>,
) -> Step {
    match op {
        KernelOp::Bin(bin) => {
            let Some((a, b)) = two_inputs(node, raw_inputs) else {
                return unsupported(node, "binary kernel needs two inputs");
            };
            let missing = missing(inputs.values, [a, b]);
            if !missing.is_empty() {
                return Step::Need(NeedSet::Nodes(missing));
            }
            bin_kernel(
                graph,
                node,
                bin,
                inputs.get(a).unwrap(),
                inputs.get(b).unwrap(),
            )
        }
        KernelOp::Not => {
            let Some(&a) = raw_inputs.first() else {
                return unsupported(node, "not kernel needs one input");
            };
            let Some(v) = inputs.get(a) else {
                return Step::Need(NeedSet::Nodes(vec![a]));
            };
            match v {
                NodeOutput::Bool(b) => Step::Ready(NodeOutput::Bool(!b)),
                _ => unsupported(node, "not kernel needs bool input"),
            }
        }
        KernelOp::If => {
            if raw_inputs.len() < 2 {
                return unsupported(node, "if kernel needs condition and branch");
            }
            let cond = raw_inputs[0];
            let Some(cond_output) = inputs.get(cond) else {
                return Step::Need(NeedSet::Nodes(vec![cond]));
            };
            let NodeOutput::Bool(take_then) = cond_output else {
                return unsupported(node, "if condition is not bool");
            };
            let selected = if *take_then {
                raw_inputs[1]
            } else if let Some(&else_node) = raw_inputs.get(2) {
                else_node
            } else {
                return Step::Ready(NodeOutput::Unit);
            };
            match inputs.get(selected) {
                Some(output) => Step::Ready(output.clone()),
                None => Step::Need(NeedSet::Nodes(vec![selected])),
            }
        }
        KernelOp::Assert => {
            let Some(&condition) = raw_inputs.first() else {
                return unsupported(node, "assert kernel needs one input");
            };
            let Some(output) = inputs.get(condition) else {
                return Step::Need(NeedSet::Nodes(vec![condition]));
            };
            match output {
                NodeOutput::Bool(true) => Step::Ready(NodeOutput::Unit),
                NodeOutput::Bool(false) => poisoned(graph, node, "assertion failed"),
                _ => unsupported(node, "assert kernel needs bool input"),
            }
        }
        KernelOp::AssertEq => {
            let Some((a, b)) = two_inputs(node, raw_inputs) else {
                return unsupported(node, "assert_eq kernel needs two inputs");
            };
            let missing = missing(inputs.values, [a, b]);
            if !missing.is_empty() {
                return Step::Need(NeedSet::Nodes(missing));
            }
            if inputs.get(a).unwrap() == inputs.get(b).unwrap() {
                Step::Ready(NodeOutput::Unit)
            } else {
                poisoned(graph, node, "assert_eq failed")
            }
        }
        KernelOp::Opaque => unsupported(node, "opaque kernel is not executable in core"),
    }
}

fn fail(graph: &DemandGraph, node: NodeId, raw_inputs: &[NodeId], inputs: ReadyInputs<'_>) -> Step {
    let Some(&message_node) = raw_inputs.first() else {
        return poisoned(graph, node, "fail");
    };
    match inputs.get(message_node) {
        Some(NodeOutput::Str(reason)) => poisoned(graph, node, reason.clone()),
        Some(_) => poisoned(graph, node, "fail"),
        None => Step::Need(NeedSet::Nodes(vec![message_node])),
    }
}

fn bin_kernel(
    graph: &DemandGraph,
    node: NodeId,
    bin: BinKind,
    a: &NodeOutput,
    b: &NodeOutput,
) -> Step {
    use BinKind::*;
    match bin {
        Add | Sub | Mul | Div | Rem | Lt | Le | Gt | Ge => {
            let (NodeOutput::U64(a), NodeOutput::U64(b)) = (a, b) else {
                return unsupported(node, "integer binary kernel needs U64 inputs");
            };
            match bin {
                Add => Step::Ready(NodeOutput::U64(a.wrapping_add(*b))),
                Sub => Step::Ready(NodeOutput::U64(a.wrapping_sub(*b))),
                Mul => Step::Ready(NodeOutput::U64(a.wrapping_mul(*b))),
                Div if *b == 0 => poisoned(graph, node, "divide by zero"),
                Div => Step::Ready(NodeOutput::U64(a / b)),
                Rem if *b == 0 => poisoned(graph, node, "remainder by zero"),
                Rem => Step::Ready(NodeOutput::U64(a % b)),
                Lt => Step::Ready(NodeOutput::Bool(a < b)),
                Le => Step::Ready(NodeOutput::Bool(a <= b)),
                Gt => Step::Ready(NodeOutput::Bool(a > b)),
                Ge => Step::Ready(NodeOutput::Bool(a >= b)),
                _ => unreachable!(),
            }
        }
        Eq | Ne => {
            let eq = a == b;
            Step::Ready(NodeOutput::Bool(if matches!(bin, Eq) { eq } else { !eq }))
        }
        And | Or => {
            let (NodeOutput::Bool(a), NodeOutput::Bool(b)) = (a, b) else {
                return unsupported(node, "boolean kernel needs bool inputs");
            };
            Step::Ready(NodeOutput::Bool(if matches!(bin, And) {
                *a && *b
            } else {
                *a || *b
            }))
        }
    }
}

fn poisoned(graph: &DemandGraph, node: NodeId, reason: impl Into<String>) -> Step {
    Step::Poison(CorePoison {
        node,
        reason: reason.into(),
        span: graph.nodes.get(node.0 as usize).and_then(|node| node.span),
    })
}

fn two_inputs(_node: NodeId, raw_inputs: &[NodeId]) -> Option<(NodeId, NodeId)> {
    match raw_inputs {
        [a, b] => Some((*a, *b)),
        _ => None,
    }
}

fn missing(
    values: &HashMap<NodeId, NodeOutput>,
    nodes: impl IntoIterator<Item = NodeId>,
) -> Vec<NodeId> {
    nodes
        .into_iter()
        .filter(|node| !values.contains_key(node))
        .collect()
}

fn const_output(value: &ConstValue) -> Option<NodeOutput> {
    match value {
        ConstValue::U64(n) => Some(NodeOutput::U64(*n)),
        ConstValue::Bool(b) => Some(NodeOutput::Bool(*b)),
        ConstValue::Str(s) => Some(NodeOutput::Str(s.clone())),
        ConstValue::Unit => Some(NodeOutput::Unit),
        ConstValue::Float(_) | ConstValue::Unsupported(_) => None,
    }
}

fn unsupported(node: NodeId, reason: impl Into<String>) -> Step {
    Step::Unsupported(UnsupportedNode {
        node,
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vx_runtime_graph::{DemandGraph, Node, NodeKind};

    #[test]
    fn lazy_data_does_not_request_fields() {
        let graph = vx_runtime_graph::example_bomb_pair();
        let values = HashMap::new();
        let step = step(&graph, NodeId(2), ReadyInputs::new(&values));
        assert!(matches!(step, Step::Ready(NodeOutput::LazyData { .. })));
    }

    #[test]
    fn projection_requests_selected_slot_only() {
        let graph = vx_runtime_graph::example_bomb_pair();
        let values = HashMap::new();
        let step = step(&graph, NodeId(3), ReadyInputs::new(&values));
        assert_eq!(step, Step::Need(NeedSet::Nodes(vec![NodeId(0)])));
    }

    #[test]
    fn if_requests_condition_before_branch() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(ConstValue::Bool(true)),
                    span: None,
                    label: "true".to_string(),
                    ty: "Bool".to_string(),
                    inputs: vec![],
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(ConstValue::U64(1)),
                    span: None,
                    label: "1".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![],
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "bomb".to_string(),
                    ty: "bottom".to_string(),
                    inputs: vec![],
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Kernel(KernelOp::If),
                    span: None,
                    label: "if".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(0), NodeId(1), NodeId(2)],
                },
            ],
            root: NodeId(3),
        };
        let mut values = HashMap::new();
        let first = step(&graph, NodeId(3), ReadyInputs::new(&values));
        assert_eq!(first, Step::Need(NeedSet::Nodes(vec![NodeId(0)])));
        values.insert(NodeId(0), NodeOutput::Bool(true));
        let second = step(&graph, NodeId(3), ReadyInputs::new(&values));
        assert_eq!(second, Step::Need(NeedSet::Nodes(vec![NodeId(1)])));
    }

    #[test]
    fn assert_eq_requests_both_inputs_then_returns_unit() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(ConstValue::U64(42)),
                    span: None,
                    label: "42".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![],
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(ConstValue::U64(42)),
                    span: None,
                    label: "42".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![],
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Kernel(KernelOp::AssertEq),
                    span: None,
                    label: "assert_eq".to_string(),
                    ty: "Unit".to_string(),
                    inputs: vec![NodeId(0), NodeId(1)],
                },
            ],
            root: NodeId(2),
        };
        let mut values = HashMap::new();
        let first = step(&graph, NodeId(2), ReadyInputs::new(&values));
        assert_eq!(
            first,
            Step::Need(NeedSet::Nodes(vec![NodeId(0), NodeId(1)]))
        );
        values.insert(NodeId(0), NodeOutput::U64(42));
        values.insert(NodeId(1), NodeOutput::U64(42));
        let second = step(&graph, NodeId(2), ReadyInputs::new(&values));
        assert_eq!(second, Step::Ready(NodeOutput::Unit));
    }
}
