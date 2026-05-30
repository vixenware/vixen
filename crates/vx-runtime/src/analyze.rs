//! The inside-out turn, for real: typed VIR → [`DemandGraph`].
//!
//! This runs the **real** front-end (parse → VIR → type-collect → resolve →
//! type-check) and walks a body's expression tree, classifying each node
//! (`runtime-semantics.md` §2). It is a **snapshot** — there is no static analysis
//! in a JIT'd language (imports resolve, packages download mid-run, `exec` outputs
//! have runtime shape), so this is one frame of a moving picture, honest about it.
//!
//! It is **partial by design**: expression kinds it does not yet handle become
//! [`NodeKind::Unresolved`] nodes (the dynamic/unhandled frontier) rather than
//! faking or crashing. Confirmed-shape kinds (literals, the bomb, tuples, binary
//! ops, index/field projection, `if`, blocks, name refs) are handled fully —
//! enough to turn `(42, 💣)[0]` and arithmetic inside out from real source. Arrays,
//! struct literals, calls, etc. are the next increment.

use std::collections::HashMap;

use vx_ir::build::{VirBuilder, VirProgram};
use vx_ir::compile::inject_implicit_stdlib_imports;
use vx_ir::expr::{ExprKind, FieldKey, LitKind};
use vx_ir::ids::ExprId;
use vx_ir::item::ItemKind;
use vx_ir::resolve::resolve_names;
use vx_ir::type_check::type_check_vir;
use vx_ir::type_collect::collect_types_vir;
use vx_parser::{SourceId, parse_source_file};

use crate::graph::{DemandGraph, Node, NodeId, NodeKind, Span};

/// Turn `entry`'s body, in `source`, inside out into a demand graph — through the
/// real front-end. Returns an error only if the source doesn't parse or the entry
/// function isn't found; unhandled expression shapes degrade to `Unresolved`.
pub fn analyze_source(source: &str, entry: &str) -> Result<DemandGraph, String> {
    let parsed = parse_source_file(source, SourceId(0));
    if !parsed.errors().is_empty() {
        return Err(format!("parse errors: {:?}", parsed.errors()));
    }
    let mut sources = vec![(SourceId(0), parsed.tree())];
    for (i, stdlib) in vx_stdlib::stdlib_sources().iter().enumerate() {
        let id = SourceId(i as u32 + 1);
        let p = parse_source_file(stdlib.text, id);
        sources.push((id, p.tree()));
    }

    let mut program = VirBuilder::build_program(&sources);
    inject_implicit_stdlib_imports(&mut program);
    let mut types = collect_types_vir(&mut program.arena);
    let _ = resolve_names(&mut program.arena);
    let _ = type_check_vir(&mut program.arena, &mut types);

    let primary = *program
        .source_units
        .first()
        .ok_or("program has no source unit")?;
    let body_id = program.arena[primary]
        .items
        .iter()
        .copied()
        .find_map(|id| match &program.arena[id].kind {
            ItemKind::Function(f) if f.name == entry => Some(f.body),
            _ => None,
        })
        .ok_or_else(|| format!("entry function `{entry}` not found"))?;
    let root_expr = program.arena[body_id].root_expr;

    let mut b = Builder {
        program: &program,
        nodes: Vec::new(),
        memo: HashMap::new(),
    };
    let root = b.walk(root_expr);
    Ok(DemandGraph {
        nodes: b.nodes,
        root,
    })
}

struct Builder<'a> {
    program: &'a VirProgram,
    nodes: Vec<Node>,
    memo: HashMap<ExprId, NodeId>,
}

impl Builder<'_> {
    fn emit(
        &mut self,
        kind: NodeKind,
        label: String,
        ty: String,
        span: Span,
        inputs: Vec<NodeId>,
    ) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            id,
            kind,
            span: Some(span),
            label,
            ty,
            inputs,
        });
        id
    }

    fn walk(&mut self, e: ExprId) -> NodeId {
        if let Some(&n) = self.memo.get(&e) {
            return n;
        }
        // Copy the shared `&VirProgram` ref so the borrow is tied to its lifetime,
        // not to `&self` — lets us recurse (`&mut self`) while reading the expr.
        let prog = self.program;
        let expr = &prog.arena[e];
        let r = expr.span.range;
        let span = Span {
            start: r.start().into(),
            end: r.end().into(),
        };
        let ty = "?".to_string();

        let id = match &expr.kind {
            ExprKind::Bomb => self.emit(NodeKind::Bomb, "💣".into(), "⊥".into(), span, vec![]),

            ExprKind::Literal(lit) => {
                let label = match &lit.value {
                    LitKind::Int { raw } | LitKind::Float { raw } => raw.clone(),
                    LitKind::String { raw } => format!("{raw:?}"),
                    LitKind::Bool(b) => b.to_string(),
                };
                self.emit(NodeKind::Const, label, ty, span, vec![])
            }

            // Tuple construction: lazy data — fields referenced, not forced.
            ExprKind::Tuple(t) => {
                let elems: Vec<ExprId> = t.elements.clone();
                let inputs = elems.into_iter().map(|x| self.walk(x)).collect();
                self.emit(NodeKind::LazyData, "(…)".into(), ty, span, inputs)
            }

            // Binary op: eager straight-line compute → a kernel.
            ExprKind::Binary(bx) => {
                let (lhs, rhs, op) = (bx.lhs, bx.rhs, format!("{:?}", bx.op));
                let l = self.walk(lhs);
                let rr = self.walk(rhs);
                self.emit(NodeKind::Kernel, op, ty, span, vec![l, rr])
            }

            // Indexing: projection. A literal-int index selects that slot.
            ExprKind::Index(ix) => {
                let (recv, idx_expr) = (ix.receiver, ix.index);
                let index = self.literal_int(idx_expr).unwrap_or(0);
                let container = self.walk(recv);
                self.emit(
                    NodeKind::Project { index },
                    format!("[{index}]"),
                    ty,
                    span,
                    vec![container],
                )
            }

            // Field access: projection. A positional field (`pair[0]` lowers to
            // `.0`) selects that slot exactly; a named field needs the struct
            // layout to map name→slot (next increment — index 0 placeholder).
            ExprKind::Field(fx) => {
                let recv = fx.receiver;
                let (index, label) = match &fx.field {
                    FieldKey::Index(i) => (*i, format!("[{i}]")),
                    FieldKey::Name(name) => (0, format!(".{name}")),
                };
                let container = self.walk(recv);
                self.emit(
                    NodeKind::Project { index },
                    label,
                    ty,
                    span,
                    vec![container],
                )
            }

            // Control flow is kernel-internal (short-circuit). Both branches are
            // drawn in the static snapshot — at runtime only one is taken.
            ExprKind::If(ifx) => {
                let (c, t, e2) = (ifx.condition, ifx.then_branch, ifx.else_branch);
                let mut inputs = vec![self.walk(c), self.walk(t)];
                if let Some(eb) = e2 {
                    inputs.push(self.walk(eb));
                }
                self.emit(NodeKind::Kernel, "if".into(), ty, span, inputs)
            }

            // A block's value is its tail; statement bindings are not yet linked
            // (let-resolution is the next increment).
            ExprKind::Block(bl) => match bl.tail {
                Some(tail) => return self.walk(tail),
                None => self.emit(NodeKind::Unresolved, "{block}".into(), ty, span, vec![]),
            },

            // Names are leaves until binding-resolution lands (next increment).
            ExprKind::NameRef(n) => self.emit(NodeKind::Param, n.name.clone(), ty, span, vec![]),

            // Honest frontier: everything else this snapshot doesn't model yet.
            other => self.emit(
                NodeKind::Unresolved,
                expr_kind_name(other).to_string(),
                ty,
                span,
                vec![],
            ),
        };
        self.memo.insert(e, id);
        id
    }

    /// If `e` is an integer literal, its value (for projection indices).
    fn literal_int(&self, e: ExprId) -> Option<u32> {
        match &self.program.arena[e].kind {
            ExprKind::Literal(lit) => match &lit.value {
                LitKind::Int { raw } => raw.parse().ok(),
                _ => None,
            },
            _ => None,
        }
    }
}

fn expr_kind_name(k: &ExprKind) -> &'static str {
    match k {
        ExprKind::Array(_) => "[array]",
        ExprKind::StructLiteral(_) => "{struct}",
        ExprKind::Record(_) => "{record}",
        ExprKind::Map(_) => "%{map}",
        ExprKind::Call(_) => "call",
        ExprKind::Where(_) => "where",
        ExprKind::Match(_) => "match",
        ExprKind::Closure(_) => "closure",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeKind;

    #[test]
    fn turns_a_bomb_tuple_projection_inside_out_from_real_source() {
        // Direct expression body — no let-binding needed. Real front-end, real VIR.
        let g = analyze_source("fn demo() -> U64 ( (42, 💣)[0] )", "demo").expect("demo analyses");

        // The bomb must be present as a Bomb node, and unreached by demand.
        let has_bomb = g.nodes.iter().any(|n| n.kind == NodeKind::Bomb);
        assert!(has_bomb, "expected a Bomb node, got: {:?}", g.nodes);

        let reached = g.demand_reachable();
        for (i, n) in g.nodes.iter().enumerate() {
            if n.kind == NodeKind::Bomb {
                assert!(
                    !reached[i],
                    "the bomb must be unreached by demand (it's pair[0])"
                );
            }
        }
    }

    #[test]
    fn positional_index_selects_the_right_slot() {
        // `(💣, 11)[1]` — the bomb is in slot 0; demanding [1] must NOT reach it.
        // This is what the FieldKey::Index extraction buys: a hardcoded index 0
        // would wrongly detonate here.
        let g = analyze_source("fn demo() -> U64 ( (💣, 11)[1] )", "demo").expect("analyses");
        let reached = g.demand_reachable();
        for (i, n) in g.nodes.iter().enumerate() {
            if n.kind == NodeKind::Bomb {
                assert!(
                    !reached[i],
                    "the bomb in slot 0 must be unreached when demanding [1]"
                );
            }
        }
    }
}
