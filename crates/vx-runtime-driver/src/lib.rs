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

    pub fn boundary_payload(node: NodeId) -> Self {
        Self {
            node,
            origin: DemandOrigin::BoundaryPayload,
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

mod private {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Sealed;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    Poison(CorePoison),
    Unsupported(UnsupportedNode),
    Boundary(NodeId),
    Stalled(NodeId),
}

pub struct Driver {
    graph: DemandGraph,
    memo: HashMap<NodeId, NodeOutput>,
    kernel_executions: u64,
}

impl Driver {
    pub fn new(graph: DemandGraph) -> Self {
        Self {
            graph,
            memo: HashMap::new(),
            kernel_executions: 0,
        }
    }

    pub fn with_seed(mut self, node: NodeId, output: NodeOutput) -> Self {
        self.memo.insert(node, output);
        self
    }

    pub fn seed(&mut self, node: NodeId, output: NodeOutput) {
        self.memo.insert(node, output);
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
        self.memo.get(&node)
    }

    pub fn drive(&mut self, request: DemandRequest) -> Result<NodeOutput, DriverError> {
        let _origin = request.origin();
        self.resolve_node(request.node())
    }

    pub fn memoized(&self, node: NodeId) -> bool {
        self.memo.contains_key(&node)
    }

    pub fn kernel_executions(&self) -> u64 {
        self.kernel_executions
    }

    fn resolve_node(&mut self, node: NodeId) -> Result<NodeOutput, DriverError> {
        if let Some(output) = self.memo.get(&node) {
            return Ok(output.clone());
        }

        loop {
            let step = vx_runtime_core::step(&self.graph, node, ReadyInputs::new(&self.memo));
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
                    self.memo.insert(node, output.clone());
                    return Ok(output);
                }
                Step::Need(NeedSet::Nodes(nodes)) => {
                    if nodes.is_empty() {
                        return Err(DriverError::Stalled(node));
                    }
                    for needed in nodes {
                        self.resolve_node(needed)?;
                    }
                }
                Step::Catch(req) => match self.resolve_node(req.input) {
                    Ok(_) => {}
                    Err(DriverError::Poison(poison)) => {
                        self.memo.insert(
                            req.caught_failure,
                            NodeOutput::Failure {
                                err: poison.err,
                                context: poison.context,
                            },
                        );
                    }
                    Err(other) => return Err(other),
                },
                Step::Boundary(req) => return Err(DriverError::Boundary(req.node)),
                Step::Poison(poison) => return Err(DriverError::Poison(poison)),
                Step::Unsupported(reason) => return Err(DriverError::Unsupported(reason)),
            }
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
}
