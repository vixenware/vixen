//! Demand graph types for Vixen runtime execution.
//!
//! This crate owns the graph artifact. It deliberately has no dependency on VIR:
//! source-shaped data is translated into this graph before evaluation.

use facet::Facet;

/// Index of a node in a [`DemandGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
pub struct NodeId(pub u32);

/// A scalar binary operation carried on a [`KernelOp::Bin`] node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum BinKind {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// The pure operation computed by a [`NodeKind::Kernel`] node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum KernelOp {
    Bin(BinKind),
    Not,
    If,
    StrLen,
    Assert,
    AssertEq,
    Opaque,
}

/// A literal value carried by a [`NodeKind::Const`] node.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum ConstValue {
    U64(u64),
    Bool(bool),
    Str(String),
    Unit,
    Float(String),
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub body: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum MatchPattern {
    Wildcard,
    Binding,
    Const(ConstValue),
    Variant { tag: String },
    Unsupported(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum HostMethod {
    Which,
    Exec,
}

/// What a graph node is.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum NodeKind {
    Const(ConstValue),
    Param,
    Kernel(KernelOp),
    LazyData,
    Project {
        index: u32,
    },
    Variant {
        tag: String,
        field_names: Vec<String>,
    },
    Closure,
    Match {
        arms: Vec<MatchArm>,
    },
    Fallible {
        caught_failure: NodeId,
    },
    CaughtFailure,
    HostMethod {
        method: HostMethod,
    },
    Boundary,
    Bomb,
    Fail,
    Unresolved,
}

/// A byte range in the source, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// A node in a demand graph.
#[derive(Debug, Clone, Facet)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub span: Option<Span>,
    pub label: String,
    pub ty: String,
    pub inputs: Vec<NodeId>,
}

/// One program frame turned into demand nodes.
#[derive(Debug, Clone, Facet)]
pub struct DemandGraph {
    pub nodes: Vec<Node>,
    pub root: NodeId,
}

impl DemandGraph {
    /// Serialize to JSON for graph-facing observability.
    pub fn to_json(&self) -> String {
        facet_json::to_string(self).expect("DemandGraph is facet-serializable")
    }

    /// Which nodes are reached by demand from the root.
    pub fn demand_reachable(&self) -> Vec<bool> {
        let mut reached = vec![false; self.nodes.len()];
        let mut stack = vec![self.root];
        while let Some(NodeId(i)) = stack.pop() {
            let i = i as usize;
            if reached[i] {
                continue;
            }
            reached[i] = true;
            let node = &self.nodes[i];
            match &node.kind {
                NodeKind::LazyData | NodeKind::Variant { .. } | NodeKind::Closure => {}
                NodeKind::Project { .. } => {
                    stack.push(node.inputs[0]);
                    if let Some(v) = self.resolved_value(NodeId(i as u32)) {
                        stack.push(v);
                    }
                }
                NodeKind::Match { .. } => {
                    if let Some(&scrutinee) = node.inputs.first() {
                        stack.push(scrutinee);
                    }
                }
                NodeKind::Fallible { .. } => {
                    if let Some(&inner) = node.inputs.first() {
                        stack.push(inner);
                    }
                }
                NodeKind::CaughtFailure => {
                    if let Some(&fallible) = node.inputs.first() {
                        stack.push(fallible);
                    }
                }
                _ => stack.extend(node.inputs.iter().copied()),
            }
        }
        reached
    }

    /// Fold projections through known lazy-data constructors.
    pub fn resolved_value(&self, n: NodeId) -> Option<NodeId> {
        let node = &self.nodes[n.0 as usize];
        match &node.kind {
            NodeKind::Project { index } => {
                let raw_container = node.inputs[0];
                let container = self.resolved_value(raw_container).unwrap_or(raw_container);
                let cnode = &self.nodes[container.0 as usize];
                match &cnode.kind {
                    NodeKind::LazyData => {
                        let slot = *cnode.inputs.get(*index as usize)?;
                        Some(self.resolved_value(slot).unwrap_or(slot))
                    }
                    _ => None,
                }
            }
            _ => Some(n),
        }
    }
}

/// Canonical smoke graph: `(42, bomb)[0]`.
pub fn example_bomb_pair() -> DemandGraph {
    DemandGraph {
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
                kind: NodeKind::Bomb,
                span: None,
                label: "bomb".to_string(),
                ty: "bottom".to_string(),
                inputs: vec![],
            },
            Node {
                id: NodeId(2),
                kind: NodeKind::LazyData,
                span: None,
                label: "(_, _)".to_string(),
                ty: "(U64, bottom)".to_string(),
                inputs: vec![NodeId(0), NodeId(1)],
            },
            Node {
                id: NodeId(3),
                kind: NodeKind::Project { index: 0 },
                span: None,
                label: "[0]".to_string(),
                ty: "U64".to_string(),
                inputs: vec![NodeId(2)],
            },
        ],
        root: NodeId(3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bomb_pair_round_trips_and_leaves_bomb_unreached() {
        let g = example_bomb_pair();
        let json = g.to_json();
        assert!(json.contains("LazyData"));
        assert!(json.contains("Bomb"));

        let reached = g.demand_reachable();
        assert!(reached[0], "const is reached through projection");
        assert!(!reached[1], "dead field is not reached");
    }
}
