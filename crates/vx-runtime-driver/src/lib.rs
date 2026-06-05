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
    #[cfg(test)]
    TestProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemandRequest {
    node: NodeId,
    origin: DemandOrigin,
    _sealed: private::Sealed,
}

impl DemandRequest {
    fn root(node: NodeId) -> Self {
        Self {
            node,
            origin: DemandOrigin::RootResult,
            _sealed: private::Sealed,
        }
    }

    fn public_value_field(node: NodeId) -> Self {
        Self {
            node,
            origin: DemandOrigin::PublicValueField,
            _sealed: private::Sealed,
        }
    }

    #[cfg(test)]
    fn test_probe(node: NodeId) -> Self {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNode {
    node: NodeId,
    output: NodeOutput,
    _sealed: private::Sealed,
}

impl ResolvedNode {
    fn new(node: NodeId, output: NodeOutput) -> Self {
        Self {
            node,
            output,
            _sealed: private::Sealed,
        }
    }

    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn output(&self) -> &NodeOutput {
        &self.output
    }

    pub fn into_output(self) -> NodeOutput {
        self.output
    }

    pub fn field_demand(&self, node: NodeId) -> Option<PublicValueDemand> {
        self.contains_field(node).then_some(PublicValueDemand {
            node,
            _sealed: private::Sealed,
        })
    }

    fn contains_field(&self, node: NodeId) -> bool {
        public_field_nodes(&self.output).contains(&node)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicValueDemand {
    node: NodeId,
    _sealed: private::Sealed,
}

impl PublicValueDemand {
    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn request(self) -> DemandRequest {
        DemandRequest::public_value_field(self.node)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct BoundaryDemand {
    node: NodeId,
    payloads: Vec<NodeId>,
    _sealed: private::Sealed,
}

impl BoundaryDemand {
    fn new(node: NodeId, payloads: Vec<NodeId>) -> Self {
        Self {
            node,
            payloads,
            _sealed: private::Sealed,
        }
    }

    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn payload(&self, node: NodeId) -> Option<BoundaryPayloadDemand> {
        self.payloads
            .contains(&node)
            .then_some(BoundaryPayloadDemand {
                node,
                _sealed: private::Sealed,
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryPayloadDemand {
    node: NodeId,
    _sealed: private::Sealed,
}

impl BoundaryPayloadDemand {
    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn request(self) -> DemandRequest {
        DemandRequest {
            node: self.node,
            origin: DemandOrigin::BoundaryPayload,
            _sealed: private::Sealed,
        }
    }

    pub fn resolve(self, resolved: ResolvedNode) -> Result<ResolvedBoundaryPayload, ResolvedNode> {
        if resolved.node == self.node {
            Ok(ResolvedBoundaryPayload {
                resolved,
                _sealed: private::Sealed,
            })
        } else {
            Err(resolved)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedBoundaryPayload {
    resolved: ResolvedNode,
    _sealed: private::Sealed,
}

impl ResolvedBoundaryPayload {
    pub fn node(&self) -> NodeId {
        self.resolved.node()
    }

    pub fn output(&self) -> &NodeOutput {
        self.resolved.output()
    }

    pub fn into_output(self) -> NodeOutput {
        self.resolved.into_output()
    }

    pub fn field_demand(&self, node: NodeId) -> Option<BoundaryPayloadDemand> {
        self.resolved
            .contains_field(node)
            .then_some(BoundaryPayloadDemand {
                node,
                _sealed: private::Sealed,
            })
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
pub struct CallCacheKey {
    pub function: String,
    pub code_identity: String,
    pub args: Vec<NodeOutput>,
}

pub trait CallCache {
    fn get(&mut self, key: &CallCacheKey) -> Option<NodeOutput>;
    fn put(&mut self, key: &CallCacheKey, output: &NodeOutput);
}

pub struct Driver {
    graph: DemandGraph,
    memo: HashMap<NodeId, NodeMemo>,
    call_memo: HashMap<CallCacheKey, NodeOutput>,
    call_cache: Option<Box<dyn CallCache>>,
    kernel_executions: u64,
}

impl Driver {
    pub fn new(graph: DemandGraph) -> Self {
        Self {
            graph,
            memo: HashMap::new(),
            call_memo: HashMap::new(),
            call_cache: None,
            kernel_executions: 0,
        }
    }

    pub fn with_call_cache(mut self, cache: impl CallCache + 'static) -> Self {
        self.call_cache = Some(Box::new(cache));
        self
    }

    pub fn with_seed(mut self, node: NodeId, output: NodeOutput) -> Self {
        self.seed(node, output);
        self
    }

    pub fn root_request(&self) -> DemandRequest {
        DemandRequest::root(self.graph.root)
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

    pub fn drive(&mut self, request: DemandRequest) -> Result<ResolvedNode, DriverError> {
        let node = request.node();
        let output = self.resolve_node(node)?;
        Ok(ResolvedNode::new(node, output))
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

        let call_memo_key = self.call_memo_key(node);
        if let Some(key) = &call_memo_key {
            if let Some(output) = self.call_memo.get(key).cloned() {
                self.memo.insert(node, NodeMemo::Ready(output.clone()));
                return Ok(output);
            }
            if let Some(output) = self.call_cache.as_mut().and_then(|cache| cache.get(key)) {
                self.call_memo.insert(key.clone(), output.clone());
                self.memo.insert(node, NodeMemo::Ready(output.clone()));
                return Ok(output);
            }
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
                        if let Some(cache) = self.call_cache.as_mut() {
                            cache.put(key, &output);
                        }
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
                    let payloads = self
                        .graph
                        .nodes
                        .get(req.node.0 as usize)
                        .map(|node| node.inputs.clone())
                        .unwrap_or_default();
                    self.clear_in_progress(node);
                    return Err(DriverError::Boundary(BoundaryDemand::new(
                        req.node, payloads,
                    )));
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

    fn call_memo_key(&self, node: NodeId) -> Option<CallCacheKey> {
        let graph_node = self.graph.nodes.get(node.0 as usize)?;
        match &graph_node.kind {
            NodeKind::Call {
                function,
                code_identity: Some(code_identity),
                arg_count,
                capture_count,
            } => {
                let key_nodes = graph_node.inputs.get(1..)?;
                let key_input_count = *arg_count as usize + *capture_count as usize;
                if key_nodes.len() != key_input_count {
                    return None;
                }
                let mut args = Vec::with_capacity(key_nodes.len());
                for &input in key_nodes {
                    args.push(self.output(input)?.clone());
                }
                Some(CallCacheKey {
                    function: function.clone(),
                    code_identity: code_identity.clone(),
                    args,
                })
            }
            _ => None,
        }
    }

    fn clear_in_progress(&mut self, node: NodeId) {
        if matches!(self.memo.get(&node), Some(NodeMemo::InProgress)) {
            self.memo.remove(&node);
        }
    }
}

fn public_field_nodes(output: &NodeOutput) -> &[NodeId] {
    match output {
        NodeOutput::LazyData { fields, .. }
        | NodeOutput::Struct { fields, .. }
        | NodeOutput::Variant { fields, .. } => fields,
        NodeOutput::U64(_)
        | NodeOutput::Bool(_)
        | NodeOutput::Str(_)
        | NodeOutput::Unit
        | NodeOutput::OutputTree
        | NodeOutput::Failure { .. }
        | NodeOutput::FailureContext(_) => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive_output(
        driver: &mut Driver,
        request: DemandRequest,
    ) -> Result<NodeOutput, DriverError> {
        driver.drive(request).map(ResolvedNode::into_output)
    }

    fn drive_root_output(driver: &mut Driver) -> Result<NodeOutput, DriverError> {
        let request = driver.root_request();
        drive_output(driver, request)
    }

    fn drive_probe_output(driver: &mut Driver, node: NodeId) -> Result<NodeOutput, DriverError> {
        drive_output(driver, DemandRequest::test_probe(node))
    }

    #[test]
    fn root_projection_skips_dead_field() {
        let graph = vx_runtime_graph::example_bomb_pair();
        let mut driver = Driver::new(graph);
        let output = drive_root_output(&mut driver).expect("root demand");
        assert_eq!(output, NodeOutput::U64(42));
        assert!(driver.memoized(NodeId(0)), "selected field memoized");
        assert!(!driver.memoized(NodeId(1)), "dead field not memoized");
    }

    #[test]
    fn resolved_public_value_authorizes_only_its_field_nodes() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(1)),
                    span: None,
                    label: "one".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(2)),
                    span: None,
                    label: "two".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::LazyData,
                    span: None,
                    label: "(one, two)".to_string(),
                    ty: "(U64, U64)".to_string(),
                    inputs: vec![NodeId(0), NodeId(1)],
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(99)),
                    span: None,
                    label: "not a field".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
            ],
            root: NodeId(2),
        };
        let mut driver = Driver::new(graph);
        let resolved = driver
            .drive(driver.root_request())
            .expect("root lazy data resolves");
        assert!(resolved.field_demand(NodeId(3)).is_none());
        let first = resolved
            .field_demand(NodeId(0))
            .expect("first field demand is authorized");
        assert_eq!(first.node(), NodeId(0));
        assert_eq!(
            drive_output(&mut driver, first.request()),
            Ok(NodeOutput::U64(1))
        );
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
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Boundary,
                    span: None,
                    label: "boundary".to_string(),
                    ty: "?".to_string(),
                    inputs: vec![NodeId(1)],
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(1)),
                    span: None,
                    label: "payload".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
            ],
            root: NodeId(0),
        };
        let mut driver = Driver::new(graph);
        let boundary = driver
            .drive(driver.root_request())
            .expect_err("root demand reaches the boundary");
        let DriverError::Boundary(boundary) = boundary else {
            panic!("expected boundary demand, got {boundary:?}");
        };
        assert_eq!(boundary.node(), NodeId(0));
        assert!(boundary.payload(NodeId(2)).is_none());
        let payload = boundary
            .payload(NodeId(1))
            .expect("declared boundary input is an authorized payload");
        assert_eq!(payload.request().origin(), DemandOrigin::BoundaryPayload);
        assert_eq!(driver.memo_state(NodeId(0)), None);

        driver.seed(NodeId(0), NodeOutput::Unit);
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::Unit));
        assert_eq!(driver.memo_state(NodeId(0)), Some(MemoState::Ready));
    }

    #[test]
    fn boundary_payload_field_is_bound_to_resolved_payload_output() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Boundary,
                    span: None,
                    label: "boundary".to_string(),
                    ty: "?".to_string(),
                    inputs: vec![NodeId(1)],
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::LazyData,
                    span: None,
                    label: "payload".to_string(),
                    ty: "(U64, U64)".to_string(),
                    inputs: vec![NodeId(2), NodeId(3)],
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(10)),
                    span: None,
                    label: "payload.first".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(20)),
                    span: None,
                    label: "payload.second".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(4),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(99)),
                    span: None,
                    label: "not a payload field".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
            ],
            root: NodeId(0),
        };
        let mut driver = Driver::new(graph);
        let boundary = driver
            .drive(driver.root_request())
            .expect_err("root demand reaches the boundary");
        let DriverError::Boundary(boundary) = boundary else {
            panic!("expected boundary demand, got {boundary:?}");
        };

        let payload = boundary
            .payload(NodeId(1))
            .expect("declared boundary input is an authorized payload");
        let unrelated = driver
            .drive(DemandRequest::test_probe(NodeId(4)))
            .expect("unrelated node resolves");
        let unrelated = payload
            .resolve(unrelated)
            .expect_err("payload demand rejects an unrelated resolved node");
        assert_eq!(unrelated.node(), NodeId(4));
        let resolved = driver
            .drive(payload.request())
            .expect("payload lazy data resolves");
        let resolved = payload
            .resolve(resolved)
            .expect("payload demand accepts the node it demanded");
        assert!(resolved.field_demand(NodeId(4)).is_none());
        let first = resolved
            .field_demand(NodeId(2))
            .expect("resolved payload field is authorized");
        assert_eq!(first.node(), NodeId(2));
        assert_eq!(
            drive_output(&mut driver, first.request()),
            Ok(NodeOutput::U64(10))
        );
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
                        capture_count: 0,
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
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
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
                        capture_count: 0,
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
                        capture_count: 0,
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
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(3)),
            Ok(NodeOutput::U64(42))
        );
        assert_eq!(driver.memo_state(NodeId(2)), None);
        assert_eq!(driver.memo_state(NodeId(3)), Some(MemoState::Ready));
    }

    #[test]
    fn calls_with_ready_arguments_share_per_run_memo() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(9)),
                    span: None,
                    label: "shared arg".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "first body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Call {
                        function: "id".to_string(),
                        code_identity: Some("fn:id".to_string()),
                        arg_count: 1,
                        capture_count: 0,
                    },
                    span: None,
                    label: "id(arg)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(1), NodeId(0)],
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "duplicate expansion tripwire".to_string(),
                    ty: "bottom".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(4),
                    kind: NodeKind::Call {
                        function: "id".to_string(),
                        code_identity: Some("fn:id".to_string()),
                        arg_count: 1,
                        capture_count: 0,
                    },
                    span: None,
                    label: "id(arg) duplicate".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(3), NodeId(0)],
                },
            ],
            root: NodeId(2),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(0)),
            Ok(NodeOutput::U64(9))
        );
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(4)),
            Ok(NodeOutput::U64(42))
        );
        assert_eq!(driver.memo_state(NodeId(3)), None);
        assert_eq!(driver.memo_state(NodeId(4)), Some(MemoState::Ready));
    }

    #[test]
    fn ready_argument_values_disambiguate_call_memo() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(9)),
                    span: None,
                    label: "arg 9".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "body 42".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Call {
                        function: "id".to_string(),
                        code_identity: Some("fn:id".to_string()),
                        arg_count: 1,
                        capture_count: 0,
                    },
                    span: None,
                    label: "id(9)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(1), NodeId(0)],
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(10)),
                    span: None,
                    label: "arg 10".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(4),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(7)),
                    span: None,
                    label: "body 7".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(5),
                    kind: NodeKind::Call {
                        function: "id".to_string(),
                        code_identity: Some("fn:id".to_string()),
                        arg_count: 1,
                        capture_count: 0,
                    },
                    span: None,
                    label: "id(10)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(4), NodeId(3)],
                },
            ],
            root: NodeId(2),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(0)),
            Ok(NodeOutput::U64(9))
        );
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(3)),
            Ok(NodeOutput::U64(10))
        );
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(5)),
            Ok(NodeOutput::U64(7))
        );
        assert_eq!(driver.memo_state(NodeId(4)), Some(MemoState::Ready));
        assert_eq!(driver.memo_state(NodeId(5)), Some(MemoState::Ready));
    }

    #[test]
    fn function_entry_disambiguates_shared_code_identity() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(9)),
                    span: None,
                    label: "shared arg".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "left body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Call {
                        function: "left".to_string(),
                        code_identity: Some("component:shared".to_string()),
                        arg_count: 1,
                        capture_count: 0,
                    },
                    span: None,
                    label: "left(arg)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(1), NodeId(0)],
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(7)),
                    span: None,
                    label: "right body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(4),
                    kind: NodeKind::Call {
                        function: "right".to_string(),
                        code_identity: Some("component:shared".to_string()),
                        arg_count: 1,
                        capture_count: 0,
                    },
                    span: None,
                    label: "right(arg)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(3), NodeId(0)],
                },
            ],
            root: NodeId(2),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(0)),
            Ok(NodeOutput::U64(9))
        );
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(4)),
            Ok(NodeOutput::U64(7))
        );
        assert_eq!(driver.memo_state(NodeId(3)), Some(MemoState::Ready));
        assert_eq!(driver.memo_state(NodeId(4)), Some(MemoState::Ready));
    }

    #[test]
    fn closure_calls_with_ready_captures_share_per_run_memo() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(5)),
                    span: None,
                    label: "capture".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(9)),
                    span: None,
                    label: "arg".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "first body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Call {
                        function: "f".to_string(),
                        code_identity: Some("closure:test".to_string()),
                        arg_count: 1,
                        capture_count: 1,
                    },
                    span: None,
                    label: "f(arg)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(2), NodeId(1), NodeId(0)],
                },
                Node {
                    id: NodeId(4),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "duplicate expansion tripwire".to_string(),
                    ty: "bottom".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(5),
                    kind: NodeKind::Call {
                        function: "f".to_string(),
                        code_identity: Some("closure:test".to_string()),
                        arg_count: 1,
                        capture_count: 1,
                    },
                    span: None,
                    label: "f(arg) duplicate".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(4), NodeId(1), NodeId(0)],
                },
            ],
            root: NodeId(3),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(0)),
            Ok(NodeOutput::U64(5))
        );
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(1)),
            Ok(NodeOutput::U64(9))
        );
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(5)),
            Ok(NodeOutput::U64(42))
        );
        assert_eq!(driver.memo_state(NodeId(4)), None);
        assert_eq!(driver.memo_state(NodeId(5)), Some(MemoState::Ready));
    }

    #[test]
    fn closure_calls_with_unresolved_captures_do_not_build_call_memo() {
        let graph = DemandGraph {
            nodes: vec![
                Node {
                    id: NodeId(0),
                    kind: NodeKind::Bomb,
                    span: None,
                    label: "cold capture".to_string(),
                    ty: "bottom".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(1),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(9)),
                    span: None,
                    label: "arg".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(2),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(42)),
                    span: None,
                    label: "first body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(3),
                    kind: NodeKind::Call {
                        function: "f".to_string(),
                        code_identity: Some("closure:test".to_string()),
                        arg_count: 1,
                        capture_count: 1,
                    },
                    span: None,
                    label: "f(arg)".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(2), NodeId(1), NodeId(0)],
                },
                Node {
                    id: NodeId(4),
                    kind: NodeKind::Const(vx_runtime_graph::ConstValue::U64(7)),
                    span: None,
                    label: "second body".to_string(),
                    ty: "U64".to_string(),
                    inputs: Vec::new(),
                },
                Node {
                    id: NodeId(5),
                    kind: NodeKind::Call {
                        function: "f".to_string(),
                        code_identity: Some("closure:test".to_string()),
                        arg_count: 1,
                        capture_count: 1,
                    },
                    span: None,
                    label: "f(arg) duplicate".to_string(),
                    ty: "U64".to_string(),
                    inputs: vec![NodeId(4), NodeId(1), NodeId(0)],
                },
            ],
            root: NodeId(3),
        };
        let mut driver = Driver::new(graph);
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(1)),
            Ok(NodeOutput::U64(9))
        );
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(5)),
            Ok(NodeOutput::U64(7))
        );
        assert_eq!(driver.memo_state(NodeId(0)), None);
        assert_eq!(driver.memo_state(NodeId(5)), Some(MemoState::Ready));
    }

    #[test]
    fn calls_with_unresolved_arguments_do_not_build_call_memo() {
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
                        capture_count: 0,
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
                        capture_count: 0,
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
        assert_eq!(drive_root_output(&mut driver), Ok(NodeOutput::U64(42)));
        assert_eq!(
            drive_probe_output(&mut driver, NodeId(5)),
            Ok(NodeOutput::U64(7))
        );
        assert_eq!(driver.memo_state(NodeId(1)), None);
        assert_eq!(driver.memo_state(NodeId(4)), None);
    }
}
