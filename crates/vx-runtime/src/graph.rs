//! The demand graph — the inside-out turn made data.
//!
//! A value is a **reference to a node** (`runtime-semantics.md` §2), so a program
//! "turned inside out" *is* a graph of nodes: constants, eager **kernels**,
//! **lazy-data** constructors, projections, **boundaries**, bombs, and the
//! **unresolved** frontier (imports / runtime-shaped values the static snapshot
//! can't see past — there is no static analysis in a JIT'd language, so this is
//! always one frame of a moving picture).
//!
//! This is the artifact the runtime evaluates, tests assert over, and the
//! playground renders — one structure, three consumers. It is facet-serialisable
//! precisely so the playground (and an LLM looking at a failing test) can read it.

use facet::Facet;

/// Index of a node in a [`DemandGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
pub struct NodeId(pub u32);

/// What a node *is* — the partition (`runtime-semantics.md` §2) made explicit.
///
/// The whole point of the inside-out turn is that this classification is visible:
/// you can look at a program and see what became an eager kernel vs a referenced
/// lazy-data node vs a boundary.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum NodeKind {
    /// A constant (literal). A trivially-evaluated node.
    Const,
    /// A parameter or binding — an input the body is given.
    Param,
    /// Eager straight-line pure compute (arithmetic, comparison, and **control
    /// flow** — `if`/`match` short-circuit *inside* a kernel). The JIT target.
    /// Its `inputs` are the values it **consumes**.
    Kernel,
    /// Lazy-data construction (tuple / array / struct / record / map). Its
    /// `inputs` are its field/element nodes — **referenced, not forced**. This is
    /// the lift out of kernels: the bomb in an undemanded field rides here.
    LazyData,
    /// Projection out of a compound (`.field`, `[index]`) — selects slot `index`
    /// of its (single) input. The thing that decides which lazy-data field
    /// actually gets demanded. **Demand flows through a `Project` to the selected
    /// slot only — never to a lazy-data node's fields by mere reference.**
    Project { index: u32 },
    /// A boundary: an intrinsic / `exec` / `acquire` / `Http` call. Effectful or
    /// non-deterministic; the kernel partition cuts here.
    Boundary,
    /// `💣` — referenced but never evaluated. Forcing it poisons. If a correct
    /// demand walk reaches it, that's a bug; in the graph it sits *dark*.
    Bomb,
    /// The dynamic frontier: an `import`, an unresolved name, or a value of
    /// runtime-determined shape the static snapshot cannot see past. Honest
    /// marker that the graph is incomplete by nature, not by laziness.
    Unresolved,
}

/// A node in the demand graph.
#[derive(Debug, Clone, Facet)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// A short, human/LLM-readable label for rendering: `"42"`, `"+"`, `"Pair"`,
    /// `".a"`, `"exec"`, `"💣"`, `"import cc"`.
    pub label: String,
    /// The node's static type, rendered. Placeholder string until bound to the
    /// real VIR type; carried because every node *is* typed.
    pub ty: String,
    /// The nodes this node references — its operands / fields / the container it
    /// projects from. Edges of the graph.
    pub inputs: Vec<NodeId>,
}

/// A demand graph: one program (or one running frame of one) turned inside out.
#[derive(Debug, Clone, Facet)]
pub struct DemandGraph {
    pub nodes: Vec<Node>,
    /// The root demand — the entry point's result node. Reachability from here is
    /// "what actually runs"; everything unreached (e.g. a bomb in a dead field)
    /// is the road not taken.
    pub root: NodeId,
}

impl DemandGraph {
    /// Serialise to JSON for the playground / a failing-test artifact. Via
    /// `facet-json` — never hand-emitted.
    pub fn to_json(&self) -> String {
        // Our own owned, derive-`Facet` types — serialisation cannot fail.
        facet_json::to_string(self).expect("DemandGraph is facet-serialisable")
    }

    /// Which nodes are reached **by demand** from the root — the "what actually
    /// runs" overlay. This is **kind-aware**, not plain input-reachability: a
    /// `LazyData` node does not demand its fields by existing (they are
    /// structural references), and a `Project` demands only the slot it selects.
    /// Everything left unreached (a bomb in an undemanded field) is the road not
    /// taken; the playground renders it dark.
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
            match node.kind {
                NodeKind::LazyData => {} // structural: fields demanded only via Project
                NodeKind::Project { index } => {
                    let container = node.inputs[0];
                    stack.push(container);
                    stack.push(self.nodes[container.0 as usize].inputs[index as usize]);
                }
                _ => stack.extend(node.inputs.iter().copied()),
            }
        }
        reached
    }
}

/// The canonical demand test — `let pair = (42, 💣); pair[0]` — turned inside out
/// by hand. Used as the first thing the playground renders and as a fixture for
/// the analysis to reproduce: a `LazyData` pair over a `Const 42` and a `Bomb`,
/// with a `Project [0]` as the root. The bomb node is **referenced but unreached
/// from the root** — exactly what the picture should show.
pub fn example_bomb_pair() -> DemandGraph {
    DemandGraph {
        nodes: vec![
            Node {
                id: NodeId(0),
                kind: NodeKind::Const,
                label: "42".to_string(),
                ty: "U64".to_string(),
                inputs: vec![],
            },
            Node {
                id: NodeId(1),
                kind: NodeKind::Bomb,
                label: "💣".to_string(),
                ty: "⊥".to_string(),
                inputs: vec![],
            },
            Node {
                id: NodeId(2),
                kind: NodeKind::LazyData,
                label: "(_, _)".to_string(),
                ty: "(U64, ⊥)".to_string(),
                inputs: vec![NodeId(0), NodeId(1)],
            },
            Node {
                id: NodeId(3),
                kind: NodeKind::Project { index: 0 },
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
    fn bomb_pair_round_trips_and_leaves_the_bomb_unreached() {
        let g = example_bomb_pair();
        let json = g.to_json();
        assert!(json.contains("LazyData"));
        assert!(json.contains("Bomb"));

        // Demand-reachability is **kind-aware** — NOT plain input-reachability.
        // The bomb is referenced by the pair yet unreached by demand.
        let reached = g.demand_reachable();
        assert!(reached[0], "Const 42 is reached via pair[0]");
        assert!(
            !reached[1],
            "the bomb is unreached — demand does not flow through lazy data"
        );
    }
}
