//! Demand driver for executable Vixen graphs.
//!
//! This crate owns external demand requests, memo storage, and the work loop. The
//! core stepper only reports missing nodes; this driver decides which external
//! requests are legal and schedules the graph until the requested output is ready.

use std::collections::HashMap;

use vx_runtime_core::{CorePoison, NeedSet, NodeOutput, ReadyInputs, Step, UnsupportedNode};
use vx_runtime_graph::{DemandGraph, Node, NodeId, NodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemandOrigin {
    RootResult,
    BoundaryPayload,
    PublicValueField,
    TestProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemandRequest {
    node: NodeId,
    origin: DemandOrigin,
    _sealed: private::Sealed,
}

impl DemandRequest {
    pub fn root(node: NodeId) -> Self {
        Self {
            node,
            origin: DemandOrigin::RootResult,
            _sealed: private::Sealed,
        }
    }

    pub fn public_value_field(node: NodeId) -> Self {
        Self {
            node,
            origin: DemandOrigin::PublicValueField,
            _sealed: private::Sealed,
        }
    }

    pub fn test_probe(node: NodeId) -> Self {
        Self {
            node,
            origin: DemandOrigin::TestProbe,
            _sealed: private::Sealed,
        }
    }

    pub fn node(self) -> NodeId {
        self.node
    }

    pub fn origin(self) -> DemandOrigin {
        self.origin
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct BoundaryDemand {
    node: NodeId,
    _sealed: private::Sealed,
}

impl BoundaryDemand {
    fn new(node: NodeId) -> Self {
        Self {
            node,
            _sealed: private::Sealed,
        }
    }

    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn payload(&self, node: NodeId) -> DemandRequest {
        DemandRequest {
            node,
            origin: DemandOrigin::BoundaryPayload,
            _sealed: private::Sealed,
        }
    }
}

mod private {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Sealed;
}

#[derive(Debug, PartialEq, Eq)]
pub enum DriverError {
    Poison(CorePoison),
    Unsupported(UnsupportedNode),
    Boundary(BoundaryDemand),
    Stalled(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoState {
    InProgress,
    Ready,
    Poison,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeMemo {
    InProgress,
    Ready(NodeOutput),
    Poison(CorePoison),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CallMemoKey {
    code_identity: String,
}

pub struct Driver {
    graph: DemandGraph,
    memo: HashMap<NodeId, NodeMemo>,
    call_memo: HashMap<CallMemoKey, NodeOutput>,
    kernel_executions: u64,
}

impl Driver {
    pub fn new(graph: DemandGraph) -> Self {
        Self {
            graph,
            memo: HashMap::new(),
            call_memo: HashMap::new(),
            kernel_executions: 0,
        }
    }

    pub fn with_seed(mut self, node: NodeId, output: NodeOutput) -> Self {
        self.seed(node, output);
        self
    }

    pub fn seed(&mut self, node: NodeId, output: NodeOutput) {
        self.memo.insert(node, NodeMemo::Ready(output));
    }

    pub fn push_node(&mut self, kind: NodeKind, label: String, inputs: Vec<NodeId>) -> NodeId {
        let id = NodeId(self.graph.nodes.len() as u32);
        self.graph.nodes.push(Node {
            id,
            kind,
            span: None,
            label,
            ty: "?".to_string(),
            inputs,
        });
        id
    }

    pub fn node(&self, node: NodeId) -> Option<&Node> {
        self.graph.nodes.get(node.0 as usize)
    }

    pub fn output(&self, node: NodeId) -> Option<&NodeOutput> {
        match self.memo.get(&node) {
            Some(NodeMemo::Ready(output)) => Some(output),
            Some(NodeMemo::InProgress | NodeMemo::Poison(_)) | None => None,
        }
    }

    pub fn drive(&mut self, request: DemandRequest) -> Result<NodeOutput, DriverError> {
        let _origin = request.origin();
        self.resolve_node(request.node())
    }

    pub fn memoized(&self, node: NodeId) -> bool {
        self.memo.contains_key(&node)
    }

    pub fn memo_state(&self, node: NodeId) -> Option<MemoState> {
        self.memo.get(&node).map(|memo| match memo {
            NodeMemo::InProgress => MemoState::InProgress,
            NodeMemo::Ready(_) => MemoState::Ready,
            NodeMemo::Poison(_) => MemoState::Poison,
        })
    }

    pub fn kernel_executions(&self) -> u64 {
        self.kernel_executions
    }

    fn resolve_node(&mut self, node: NodeId) -> Result<NodeOutput, DriverError> {
        match self.memo.get(&node) {
            Some(NodeMemo::Ready(output)) => return Ok(output.clone()),
            Some(NodeMemo::Poison(poison)) => return Err(DriverError::Poison(poison.clone())),
            Some(NodeMemo::InProgress) => return Err(DriverError::Stalled(node)),
            None => {}
        }

        let call_memo_key = self.zero_arg_call_memo_key(node);
        if let Some(output) = call_memo_key
            .as_ref()
            .and_then(|key| self.call_memo.get(key).cloned())
        {
            self.memo.insert(node, NodeMemo::Ready(output.clone()));
            return Ok(output);
        }

        self.memo.insert(node, NodeMemo::InProgress);
        loop {
            let ready_inputs = self.ready_outputs();
            let step = vx_runtime_core::step(&self.graph, node, ReadyInputs::new(&ready_inputs));
            match step {
                Step::Ready(output) => {
                    if self
                        .graph
                        .nodes
                        .get(node.0 as usize)
                        .is_some_and(|n| matches!(n.kind, NodeKind::Kernel(_)))
                    {
                        self.kernel_executions += 1;
                    }
                    if let Some(key) = &call_memo_key {
                        self.call_memo.insert(key.clone(), output.clone());
                    }
                    self.memo.insert(node, NodeMemo::Ready(output.clone()));
                    return Ok(output);
                }
                Step::Need(NeedSet::Nodes(nodes)) => {
                    if nodes.is_empty() {
                        self.clear_in_progress(node);
                        return Err(DriverError::Stalled(node));
                    }
                    for needed in nodes {
                        if let Err(error) = self.resolve_node(needed) {
                            self.clear_in_progress(node);
                            return Err(error);
                        }
                    }
                }
                Step::Catch(req) => match self.resolve_node(req.input) {
                    Ok(_) => {}
                    Err(DriverError::Poison(poison)) => {
                        self.memo.insert(
                            req.caught_failure,
                            NodeMemo::Ready(NodeOutput::Failure {
                                err: poison.err,
                                context: poison.context,
                            }),
                        );
                    }
                    Err(other) => {
                        self.clear_in_progress(node);
                        return Err(other);
                    }
                },
                Step::Boundary(req) => {
                    self.clear_in_progress(node);
                    return Err(DriverError::Boundary(BoundaryDemand::new(req.node)));
                }
                Step::Poison(poison) => {
                    self.memo.insert(node, NodeMemo::Poison(poison.clone()));
                    return Err(DriverError::Poison(poison));
                }
                Step::Unsupported(reason) => {
                    self.clear_in_progress(node);
                    return Err(DriverError::Unsupported(reason));
                }
            }
        }
    }

    fn ready_outputs(&self) -> HashMap<NodeId, NodeOutput> {
        self.memo
            .iter()
            .filter_map(|(&node, memo)| match memo {
                NodeMemo::Ready(output) => Some((node, output.clone())),
                NodeMemo::InProgress | NodeMemo::Poison(_) => None,
            })
            .collect()
    }

    fn zero_arg_call_memo_key(&self, node: NodeId) -> Option<CallMemoKey> {
        let graph_node = self.graph.nodes.get(node.0 as usize)?;
        match &graph_node.kind {
            NodeKind::Call {
                code_identity: Some(code_identity),
                arg_count: 0,
                ..
            } => Some(CallMemoKey {
                code_identity: code_identity.clone(),
            }),
            _ => None,
        }
    }

    fn clear_in_progress(&mut self, node: NodeId) {
        if matches!(self.memo.get(&node), Some(NodeMemo::InProgress)) {
            self.memo.remove(&node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_projection_skips_dead_field() {
        let graph = vx_runtime_graph::example_bomb_pair();
        let mut driver = Driver::new(graph);
        let output = driver
            .drive(DemandRequest::root(NodeId(3)))
            .expect("root demand");
        assert_eq!(output, NodeOutput::U64(42));
        assert!(driver.memoized(NodeId(0)), "selected field memoized");
        assert!(!driver.memoized(NodeId(1)), "dead field not memoized");
    }

    #[test]
    fn demanded_bomb_poisons() {
        let graph = vx_runtime_graph::example_bomb_pair();
        let mut driver = Driver::new(graph);
        let err = driver
            .drive(DemandRequest::test_probe(NodeId(1)))
            .expect_err("bomb demand poisons");
        match err {
            DriverError::Poison(p) => assert_eq!(p.node, NodeId(1)),
            other => panic!("expected poison, got {other:?}"),
        }
    }

    #[test]
    fn demanded_bomb_memoizes_poison() {
        let graph = vx_runtime_graph::example_bomb_pair();
        let mut driver = Driver::new(graph);
        let first = driver
            .drive(DemandRequest::test_probe(NodeId(1)))
            .expect_err("first bomb demand poisons");
        assert_eq!(driver.memo_state(NodeId(1)), Some(MemoState::Poison));

        let second = driver
            .drive(DemandRequest::test_probe(NodeId(1)))
            .expect_err("memoized bomb demand poisons again");
        assert_eq!(second, first);
    }

    #[test]
    fn boundary_request_does_not_leave_node_in_progress() {
        let graph = DemandGraph {
            nodes: vec![Node {
                id: NodeId(0),
                kind: NodeKind::Boundary,
                span: None,
                label: "boundary".to_string(),
                ty: "?".to_string(),
                inputs: Vec::new(),
            }],
            root: NodeId(0),
        };
        let mut driver = Driver::new(graph);
        let boundary = driver
            .drive(DemandRequest::root(NodeId(0)))
            .expect_err("root demand reaches the boundary");
        let DriverError::Boundary(boundary) = boundary else {
            panic!("expected boundary demand, got {boundary:?}");
        };
        assert_eq!(boundary.node(), NodeId(0));
        assert_eq!(
            boundary.payload(NodeId(1)).origin(),
            DemandOrigin::BoundaryPayload
        );
        assert_eq!(driver.memo_state(NodeId(0)), None);

        driver.seed(NodeId(0), NodeOutput::Unit);
        assert_eq!(
            driver.drive(DemandRequest::root(NodeId(0))),
            Ok(NodeOutput::Unit)
        );
        assert_eq!(driver.memo_state(NodeId(0)), Some(MemoState::Ready));
    }

    #[test]
    fn call_node_demands_expanded_body_not_argument_edges() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "body root".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "dead arg".to_string(),
                    ty: "bottom".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Call {
                        function: "pick_first".to_string(),
                        code_identity: Some("fn:pick_first".to_string()),
                        arg_count: 1,
                    },
                    span: None,
                    label: "pick_first(...)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(0), NodeId(1)],
                },
            ],
            root: NodeId(2),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            driver.drive(DemandRequest::root(NodeId(2))),
            Ok(NodeOutput::U64(42))
        );
        assert_eq!(driver.memo_state(NodeId(0)), Some(MemoState::Ready));
        assert_eq!(driver.memo_state(NodeId(1)), None);
        assert_eq!(driver.memo_state(NodeId(2)), Some(MemoState::Ready));
    }

    #[test]
    fn zero_arg_call_nodes_share_per_run_memo() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "answer body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Call {
                        function: "answer".to_string(),
                        code_identity: Some("fn:answer".to_string()),
                        arg_count: 0,
                    },
                    span: None,
                    label: "answer()".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(0)],
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "duplicate expansion tripwire".to_string(),
                    ty: "bottom".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Call {
                        function: "answer".to_string(),
                        code_identity: Some("fn:answer".to_string()),
                        arg_count: 0,
                    },
                    span: None,
                    label: "answer() duplicate".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(2)],
                },
            ],
            root: NodeId(1),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            driver.drive(DemandRequest::root(NodeId(1))),
            Ok(NodeOutput::U64(42))
        );
        assert_eq!(
            driver.drive(DemandRequest::root(NodeId(3))),
            Ok(NodeOutput::U64(42))
        );
        assert_eq!(driver.memo_state(NodeId(2)), None);
        assert_eq!(driver.memo_state(NodeId(3)), Some(MemoState::Ready));
    }

    #[test]
    fn calls_with_arguments_do_not_use_zero_arg_memo() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "first body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "first dead arg".to_string(),
                    ty: "bottom".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Call {
                        function: "id".to_string(),
                        code_identity: Some("fn:id".to_string()),
                        arg_count: 1,
                    },
                    span: None,
                    label: "id(42)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(0), NodeId(1)],
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(7)),
                    span: None,
                    label: "second body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(4),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "second dead arg".to_string(),
                    ty: "bottom".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(5),
                    kind: NodeKind::Call {
                        function: "id".to_string(),
                        code_identity: Some("fn:id".to_string()),
                        arg_count: 1,
                    },
                    span: None,
                    label: "id(7)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(3), NodeId(4)],
                },
            ],
            root: NodeId(2),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            driver.drive(DemandRequest::root(NodeId(2))),
            Ok(NodeOutput::U64(42))
        );
        assert_eq!(
            driver.drive(DemandRequest::root(NodeId(5))),
            Ok(NodeOutput::U64(7))
        );
        assert_eq!(driver.memo_state(NodeId(1)), None);
        assert_eq!(driver.memo_state(NodeId(4)), None);
    }
}
