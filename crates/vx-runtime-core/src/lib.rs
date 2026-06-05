//! Graph stepper for Vixen runtime nodes.
//!
//! This crate is intentionally source-blind. It can inspect graph nodes and
//! report missing dependencies, but it does not own scheduling, memo storage, or
//! external demand origins.

use std::collections::HashMap;

use vx_runtime_graph::{
    BinKind, ConstValue, DemandGraph, KernelOp, MatchPattern, NodeId, NodeKind, Span,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeOutput {
    U64(u64),
    Bool(bool),
    Str(String),
    Unit,
    CasTree([u8; 32]),
    OutputTree,
    LazyData {
        label: String,
        fields: Vec<NodeId>,
    },
    Struct {
        field_names: Vec<String>,
        fields: Vec<NodeId>,
    },
    Variant {
        tag: String,
        field_names: Vec<String>,
        fields: Vec<NodeId>,
    },
    Failure {
        err: Box<NodeOutput>,
        context: CoreFailureContext,
    },
    FailureContext(CoreFailureContext),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CoreFailureContext {
    pub span: Option<Span>,
    pub demand_path: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorePoison {
    pub node: NodeId,
    pub reason: String,
    pub span: Option<Span>,
    pub err: Box<NodeOutput>,
    pub context: CoreFailureContext,
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
pub struct CatchRequest {
    pub input: NodeId,
    pub caught_failure: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedSet {
    Nodes(Vec<NodeId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Need(NeedSet),
    Ready(NodeOutput),
    Catch(CatchRequest),
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
        NodeKind::Variant { tag, field_names } => Step::Ready(NodeOutput::Variant {
            tag: tag.clone(),
            field_names: field_names.clone(),
            fields: n.inputs.clone(),
        }),
        NodeKind::Project { index } => project(graph, node, n.inputs.as_slice(), *index, inputs),
        NodeKind::Kernel(op) => kernel(graph, node, n.inputs.as_slice(), *op, inputs),
        NodeKind::Closure => unsupported(node, "closure value is not executable in core yet"),
        NodeKind::Call { .. } => call(node, n.inputs.as_slice(), inputs),
        NodeKind::Match { arms } => match_node(node, n.inputs.as_slice(), arms, inputs),
        NodeKind::Fallible { caught_failure } => {
            fallible(node, n.inputs.as_slice(), *caught_failure, inputs)
        }
        NodeKind::CaughtFailure => caught_failure(node, n.inputs.as_slice(), inputs),
        NodeKind::HostMethod { .. } => boundary_after_inputs(node, n.inputs.as_slice(), inputs),
        NodeKind::Boundary => Step::Boundary(BoundaryRequest { node }),
        NodeKind::Bomb => poisoned(graph, node, "bomb demanded"),
        NodeKind::Fail => fail(graph, node, n.inputs.as_slice(), inputs),
        NodeKind::Unresolved => unsupported(node, format!("unresolved node `{}`", n.label)),
    }
}

fn call(node: NodeId, raw_inputs: &[NodeId], inputs: ReadyInputs<'_>) -> Step {
    let Some(&body_root) = raw_inputs.first() else {
        return unsupported(node, "call node has no expanded body root");
    };
    match inputs.get(body_root) {
        Some(output) => Step::Ready(output.clone()),
        None => Step::Need(NeedSet::Nodes(vec![body_root])),
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
    let fields = match container_output {
        NodeOutput::LazyData { fields, .. }
        | NodeOutput::Struct { fields, .. }
        | NodeOutput::Variant { fields, .. } => fields,
        NodeOutput::Failure { err, context } => {
            return match index {
                0 => Step::Ready((**err).clone()),
                1 => Step::Ready(NodeOutput::FailureContext(context.clone())),
                _ => unsupported(node, "failure project index out of bounds"),
            };
        }
        _ => {
            return unsupported(
                node,
                "project container is not lazy data, struct, variant, or failure",
            );
        }
    };
    let Some(&field) = fields.get(index as usize) else {
        return unsupported(node, "project index out of bounds");
    };
    match inputs.get(field) {
        Some(output) => Step::Ready(output.clone()),
        None => Step::Need(NeedSet::Nodes(vec![field])),
    }
}

fn match_node(
    node: NodeId,
    raw_inputs: &[NodeId],
    arms: &[vx_runtime_graph::MatchArm],
    inputs: ReadyInputs<'_>,
) -> Step {
    let Some(&scrutinee) = raw_inputs.first() else {
        return unsupported(node, "match node has no scrutinee");
    };
    let Some(scrutinee_output) = inputs.get(scrutinee) else {
        return Step::Need(NeedSet::Nodes(vec![scrutinee]));
    };
    let Some(arm) = arms
        .iter()
        .find(|arm| pattern_matches(&arm.pattern, scrutinee_output))
    else {
        return unsupported(node, "match had no matching arm");
    };
    match inputs.get(arm.body) {
        Some(output) => Step::Ready(output.clone()),
        None => Step::Need(NeedSet::Nodes(vec![arm.body])),
    }
}

fn fallible(
    node: NodeId,
    raw_inputs: &[NodeId],
    caught_failure: NodeId,
    inputs: ReadyInputs<'_>,
) -> Step {
    let Some(&inner) = raw_inputs.first() else {
        return unsupported(node, "fallible node has no inner expression");
    };
    if inputs.get(caught_failure).is_some() {
        return Step::Ready(NodeOutput::Variant {
            tag: "Err".to_string(),
            field_names: vec!["0".to_string()],
            fields: vec![caught_failure],
        });
    }
    if inputs.get(inner).is_some() {
        return Step::Ready(NodeOutput::Variant {
            tag: "Ok".to_string(),
            field_names: vec!["0".to_string()],
            fields: vec![inner],
        });
    }
    Step::Catch(CatchRequest {
        input: inner,
        caught_failure,
    })
}

fn caught_failure(node: NodeId, raw_inputs: &[NodeId], inputs: ReadyInputs<'_>) -> Step {
    let Some(&fallible) = raw_inputs.first() else {
        return unsupported(node, "caught-failure node has no fallible owner");
    };
    if inputs.get(fallible).is_some() {
        return unsupported(node, "fallible expression did not catch a failure");
    }
    Step::Need(NeedSet::Nodes(vec![fallible]))
}

fn boundary_after_inputs(node: NodeId, raw_inputs: &[NodeId], inputs: ReadyInputs<'_>) -> Step {
    let missing = missing(inputs.values, raw_inputs.iter().copied());
    if missing.is_empty() {
        Step::Boundary(BoundaryRequest { node })
    } else {
        Step::Need(NeedSet::Nodes(missing))
    }
}

fn pattern_matches(pattern: &MatchPattern, output: &NodeOutput) -> bool {
    match pattern {
        MatchPattern::Wildcard | MatchPattern::Binding => true,
        MatchPattern::Const(value) => const_matches(value, output),
        MatchPattern::Variant { tag } => match output {
            NodeOutput::Variant {
                tag: actual_tag, ..
            } => actual_tag == tag,
            _ => false,
        },
        MatchPattern::Unsupported(_) => false,
    }
}

fn const_matches(value: &ConstValue, output: &NodeOutput) -> bool {
    match (value, output) {
        (ConstValue::U64(a), NodeOutput::U64(b)) => *a == *b,
        (ConstValue::Bool(a), NodeOutput::Bool(b)) => *a == *b,
        (ConstValue::Str(a), NodeOutput::Str(b)) => a == b,
        (ConstValue::Unit, NodeOutput::Unit) => true,
        _ => false,
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
        KernelOp::StrLen => {
            let Some(&receiver) = raw_inputs.first() else {
                return unsupported(node, "string length kernel needs one receiver");
            };
            let Some(output) = inputs.get(receiver) else {
                return Step::Need(NeedSet::Nodes(vec![receiver]));
            };
            match output {
                NodeOutput::Str(s) => Step::Ready(NodeOutput::U64(s.len() as u64)),
                _ => unsupported(node, "string length kernel needs string receiver"),
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
        Some(NodeOutput::Failure { err, context }) => {
            poison_with_context(node, (**err).clone(), context.clone())
        }
        Some(output) => poison_with_err(graph, node, output.clone()),
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
    let reason = reason.into();
    poison_with_err(graph, node, NodeOutput::Str(reason))
}

fn poison_with_err(graph: &DemandGraph, node: NodeId, err: NodeOutput) -> Step {
    let span = graph.nodes.get(node.0 as usize).and_then(|node| node.span);
    let context = CoreFailureContext {
        span,
        demand_path: Vec::new(),
    };
    poison_with_context(node, err, context)
}

fn poison_with_context(node: NodeId, err: NodeOutput, context: CoreFailureContext) -> Step {
    let reason = output_reason(&err);
    Step::Poison(CorePoison {
        node,
        reason,
        span: context.span,
        err: Box::new(err),
        context,
    })
}

fn output_reason(output: &NodeOutput) -> String {
    match output {
        NodeOutput::Str(s) => s.clone(),
        NodeOutput::U64(n) => n.to_string(),
        NodeOutput::Bool(b) => b.to_string(),
        NodeOutput::Unit => "()".to_string(),
        NodeOutput::CasTree(hash) => {
            let mut hex = String::with_capacity(64);
            for byte in hash {
                hex.push_str(&format!("{byte:02x}"));
            }
            hex
        }
        NodeOutput::OutputTree => "output tree".to_string(),
        NodeOutput::LazyData { label, .. } => label.clone(),
        NodeOutput::Struct { .. } => "struct".to_string(),
        NodeOutput::Variant { tag, .. } => tag.clone(),
        NodeOutput::Failure { err, .. } => output_reason(err),
        NodeOutput::FailureContext(_) => "failure context".to_string(),
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
