//! Graph stepper for Vixen runtime nodes.
//!
//! This crate is intentionally source-blind. It can inspect graph nodes and
//! report missing dependencies, but it does not own scheduling, memo storage, or
//! external demand origins.

use std::collections::HashMap;

use vx_runtime_graph::{BinKind, DemandGraph, KernelOp, NodeId, NodeKind};

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
        NodeKind::Const => match const_output(&n.label) {
            Some(output) => Step::Ready(output),
            None => unsupported(node, format!("unsupported const `{}`", n.label)),
        },
        NodeKind::Param => unsupported(node, "parameter has no supplied value"),
        NodeKind::LazyData => Step::Ready(NodeOutput::LazyData {
            label: n.label.clone(),
            fields: n.inputs.clone(),
        }),
        NodeKind::Project { index } => project(graph, node, n.inputs.as_slice(), *index, inputs),
        NodeKind::Kernel(op) => kernel(graph, node, n.inputs.as_slice(), *op, inputs),
        NodeKind::Boundary => Step::Boundary(BoundaryRequest { node }),
        NodeKind::Bomb => Step::Poison(CorePoison {
            node,
            reason: "bomb demanded".to_string(),
        }),
        NodeKind::Fail => fail(node, n.inputs.as_slice(), inputs),
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
    _graph: &DemandGraph,
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
            bin_kernel(node, bin, inputs.get(a).unwrap(), inputs.get(b).unwrap())
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
        KernelOp::Opaque => unsupported(node, "opaque kernel is not executable in core"),
    }
}

fn fail(node: NodeId, raw_inputs: &[NodeId], inputs: ReadyInputs<'_>) -> Step {
    let Some(&message_node) = raw_inputs.first() else {
        return Step::Poison(CorePoison {
            node,
            reason: "fail".to_string(),
        });
    };
    match inputs.get(message_node) {
        Some(NodeOutput::Str(reason)) => Step::Poison(CorePoison {
            node,
            reason: reason.clone(),
        }),
        Some(_) => Step::Poison(CorePoison {
            node,
            reason: "fail".to_string(),
        }),
        None => Step::Need(NeedSet::Nodes(vec![message_node])),
    }
}

fn bin_kernel(node: NodeId, bin: BinKind, a: &NodeOutput, b: &NodeOutput) -> Step {
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
                Div if *b == 0 => Step::Poison(CorePoison {
                    node,
                    reason: "divide by zero".to_string(),
                }),
                Div => Step::Ready(NodeOutput::U64(a / b)),
                Rem if *b == 0 => Step::Poison(CorePoison {
                    node,
                    reason: "remainder by zero".to_string(),
                }),
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

fn const_output(label: &str) -> Option<NodeOutput> {
    if let Ok(n) = label.parse::<u64>() {
        return Some(NodeOutput::U64(n));
    }
    match label {
        "true" => Some(NodeOutput::Bool(true)),
        "false" => Some(NodeOutput::Bool(false)),
        "()" => Some(NodeOutput::Unit),
        _ if label.len() >= 2 && label.starts_with('"') && label.ends_with('"') => {
            Some(NodeOutput::Str(unquote(&label[1..label.len() - 1])))
        }
        _ => None,
    }
}

fn unquote(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
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
                    kind: NodeKind::Const,
                    span: None,
                    label: "true".to_string(),
                    ty: "Bool".to_string(),
                    inputs: vec![],
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const,
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
}
