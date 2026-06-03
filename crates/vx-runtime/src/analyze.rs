//! The inside-out turn, for real: typed VIR → [`DemandGraph`].
//!
//! Runs the **real** front-end (parse → VIR → type-collect → resolve →
//! type-check) and walks a body's expression tree, classifying each node
//! (the demand-driven runtime, wiki). It is a **snapshot** — there is no static analysis
//! in a JIT'd language (imports resolve, packages download mid-run, `exec` outputs
//! have runtime shape), so this is one frame of a moving picture, honest about it.
//!
//! **Partial by design:** kinds it doesn't model yet become [`NodeKind::Unresolved`]
//! nodes — the honest frontier, never faked. Handled today: literals, the bomb,
//! tuples/arrays/structs/records (lazy data), binary ops & calls (kernels /
//! boundaries), index & field projection (with real positional *and* named-field
//! slot resolution), `if` (control flow stays in-kernel), blocks with **`let`
//! bindings linked to their value subgraphs**, and parameters. So
//! `let pair = (42, 💣); pair[0]` and `Pair { a: 42, b: 💣 }.a` turn inside out
//! correctly from real source — the bomb stays unreached for the right reason.

use std::collections::HashMap;

use vx_ir::build::VirProgram;
use vx_ir::expr::{
    ArrayElement, BinOp, ExprKind, FieldKey, LitKind, MapElement, PrefixOp, StringPart,
};
use vx_ir::ids::{BindingId, BodyId, ExprId, PatternId};
use vx_ir::item::ItemKind;
use vx_ir::pattern::PatternKind;
use vx_ir::scope::DefTarget;
use vx_ir::stmt::StmtKind;
use vx_types::{TypeEnv, TypeId};

use crate::graph::{BinKind, DemandGraph, KernelOp, Node, NodeId, NodeKind, Span};

/// Intrinsic/boundary call names — effectful or non-deterministic, so the kernel
/// partition cuts here (a call to one of these is a `Boundary`, not a `Kernel`).
///
/// `discover` is deliberately **absent**: it is *deprecated* — host facts now come
/// from standing capability advertisement + predicate-based executor selection
/// (`docs/design/runners-and-control-plane.md` §6), not an inline `discover`.
/// `discover` is not a target; any remaining `discover(...)` call simply falls
/// through as an ordinary call here.
const BOUNDARY_CALLS: &[&str] = &["exec", "acquire", "Http", "http"];

/// Turn `entry`'s body, in `source`, inside out — through the real front-end.
/// Errors only if the source doesn't parse or the entry isn't found; unhandled
/// expression shapes degrade to `Unresolved`.
pub fn analyze_source(source: &str, entry: &str) -> Result<DemandGraph, String> {
    let (program, env) = front_end(source)?;
    let body_id =
        find_entry(&program, entry).ok_or_else(|| format!("entry function `{entry}` not found"))?;
    Ok(build_graph(&program, &env, body_id))
}

/// Analyze **every** top-level function in `source` — `(name, graph)` per fn, in
/// declaration order. The playground uses this to let you click through a file.
pub fn analyze_all(source: &str) -> Result<Vec<(String, DemandGraph)>, String> {
    let (program, env) = front_end(source)?;
    let primary = *program
        .source_units
        .first()
        .ok_or("program has no source unit")?;
    let out = program.arena[primary]
        .items
        .iter()
        .copied()
        .filter_map(|id| match &program.arena[id].kind {
            ItemKind::Function(f) => Some((f.name.clone(), build_graph(&program, &env, f.body))),
            _ => None,
        })
        .collect();
    Ok(out)
}

/// Run the engine's front-end on `source` and return the checked program + its
/// type environment. This drives the **one** shared semantic pipeline
/// ([`vx_ir::compile::semantic_front_end`]): parse → VIR → prelude → resolve →
/// type-check → capability-check, with packages + grammars loaded. vx-runtime is
/// the engine that owns this entry; vx-lsp and vx-diagnostics ride the same passes.
///
/// Strict: a `CompileError` (parse / resolve / type / capability) comes back as a
/// setup-error string — the demand evaluator only runs a well-formed program.
pub(crate) fn front_end(source: &str) -> Result<(VirProgram, TypeEnv), String> {
    let fe = vx_ir::compile::semantic_front_end_bundled(source).map_err(|e| e.to_string())?;
    Ok((fe.program, fe.env))
}

fn find_entry(program: &VirProgram, entry: &str) -> Option<BodyId> {
    let primary = *program.source_units.first()?;
    program.arena[primary]
        .items
        .iter()
        .copied()
        .find_map(|id| match &program.arena[id].kind {
            ItemKind::Function(f) if f.name == entry => Some(f.body),
            _ => None,
        })
}

/// Turn one body inside out into a demand graph.
pub(crate) fn build_graph(program: &VirProgram, env: &TypeEnv, body_id: BodyId) -> DemandGraph {
    build_graph_indexed(program, env, body_id).0
}

/// Like [`build_graph`], but also returns two indices the evaluator needs to
/// dispatch scalar *sub-expressions* to kernels: the expression→node map (which
/// graph node a given `ExprId` produced) and, for every `Param` node, the binding
/// it came from (so a kernel parameter maps back to the environment).
pub(crate) fn build_graph_indexed(
    program: &VirProgram,
    env: &TypeEnv,
    body_id: BodyId,
) -> (
    DemandGraph,
    HashMap<ExprId, NodeId>,
    HashMap<NodeId, BindingId>,
) {
    let params: Vec<PatternId> = program.arena[body_id]
        .params
        .iter()
        .map(|p| p.pattern)
        .collect();
    let root_expr = program.arena[body_id].root_expr;
    let mut b = Builder {
        program,
        env,
        nodes: Vec::new(),
        memo: HashMap::new(),
        bindings: HashMap::new(),
        field_names: HashMap::new(),
    };
    for pat in params {
        b.bind_param(pat);
    }
    let root = b.walk(root_expr);
    // Invert the binding map for `Param` nodes only (a kernel's parameters are
    // always `Param` nodes; let-bindings link to their value node instead).
    let mut param_bindings = HashMap::new();
    for (&binding, &node) in &b.bindings {
        if matches!(b.nodes[node.0 as usize].kind, NodeKind::Param) {
            param_bindings.insert(node, binding);
        }
    }
    let graph = DemandGraph {
        nodes: b.nodes,
        root,
    };
    (graph, b.memo, param_bindings)
}

struct Builder<'a> {
    program: &'a VirProgram,
    env: &'a TypeEnv,
    nodes: Vec<Node>,
    memo: HashMap<ExprId, NodeId>,
    /// Local binding → the node of its value (the link that makes `pair[0]` reach
    /// the construction). Flat (shadowing not modelled yet).
    bindings: HashMap<BindingId, NodeId>,
    /// For a lazy-data node built from a named struct/record: its field names in
    /// slot order, so a named projection (`.a`) can resolve to the right slot.
    field_names: HashMap<NodeId, Vec<String>>,
}

impl Builder<'_> {
    fn emit(&mut self, kind: NodeKind, label: String, span: Span, inputs: Vec<NodeId>) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            id,
            kind,
            span: Some(span),
            label,
            ty: "?".to_string(),
            inputs,
        });
        id
    }

    /// Bind a parameter pattern to a fresh `Param` node, so name refs to it link.
    fn bind_param(&mut self, pat: PatternId) {
        let prog = self.program;
        let p = &prog.arena[pat];
        if let PatternKind::Binding(pb) = &p.kind {
            let (name, binding) = (pb.name.clone(), pb.binding);
            let span = to_span(p.span);
            let node = self.emit(NodeKind::Param, name, span, vec![]);
            self.bindings.insert(binding, node);
        }
    }

    fn walk(&mut self, e: ExprId) -> NodeId {
        if let Some(&n) = self.memo.get(&e) {
            return n;
        }
        // Copy the shared `&VirProgram` ref so the borrow ties to its lifetime, not
        // to `&self` — lets us recurse (`&mut self`) while reading the expr.
        let prog = self.program;
        let expr = &prog.arena[e];
        let span = to_span(expr.span);
        let ty_str = self.render_ty(expr.ty);

        let id = match &expr.kind {
            ExprKind::Bomb => self.emit(NodeKind::Bomb, "💣".into(), span, vec![]),

            ExprKind::Literal(lit) => {
                let label = match &lit.value {
                    LitKind::Int { raw } | LitKind::Float { raw } => raw.clone(),
                    LitKind::String { raw } => format!("{raw:?}"),
                    LitKind::Bool(b) => b.to_string(),
                };
                self.emit(NodeKind::Const, label, span, vec![])
            }

            // Lazy data — fields referenced, not forced.
            ExprKind::Tuple(t) => {
                let elems = t.elements.clone();
                let inputs = elems.into_iter().map(|x| self.walk(x)).collect();
                self.emit(NodeKind::LazyData, "(…)".into(), span, inputs)
            }
            ExprKind::Array(a) => {
                let exprs: Vec<ExprId> = a
                    .elements
                    .iter()
                    .map(|el| match el {
                        ArrayElement::Expr(x) | ArrayElement::Spread(x) => *x,
                    })
                    .collect();
                let inputs = exprs.into_iter().map(|x| self.walk(x)).collect();
                self.emit(NodeKind::LazyData, "[…]".into(), span, inputs)
            }
            ExprKind::StructLiteral(s) => {
                let label = s.name.clone();
                let pairs: Vec<(String, ExprId)> =
                    s.fields.iter().map(|f| (f.name.clone(), f.value)).collect();
                self.lazy_record(label, pairs, span)
            }
            ExprKind::Record(r) => {
                let pairs: Vec<(String, ExprId)> =
                    r.fields.iter().map(|f| (f.name.clone(), f.value)).collect();
                self.lazy_record("{…}".into(), pairs, span)
            }

            // Eager straight-line compute → a kernel.
            ExprKind::Binary(bx) => {
                let (lhs, rhs) = (bx.lhs, bx.rhs);
                let (label, kop) = (format!("{:?}", bx.op), bin_kind(bx.op));
                let l = self.walk(lhs);
                let rr = self.walk(rhs);
                self.emit(NodeKind::Kernel(kop), label, span, vec![l, rr])
            }
            ExprKind::Prefix(px) => {
                let kop = match px.op {
                    PrefixOp::Not => KernelOp::Not,
                    PrefixOp::Neg => KernelOp::Opaque, // scalar negation: deferred
                };
                let operand = self.walk(px.operand);
                self.emit(
                    NodeKind::Kernel(kop),
                    format!("{:?}", px.op),
                    span,
                    vec![operand],
                )
            }

            // A call: kernel (pure) or boundary (intrinsic/effect). Args are inputs;
            // walking them is how `assert_eq(pair[0], 42)` reaches `pair[0]`.
            ExprKind::Call(c) => {
                let callee = c.callee;
                let args: Vec<ExprId> = c.args.iter().map(|a| a.expr).collect();
                let callee_name = match &prog.arena[callee].kind {
                    ExprKind::NameRef(n) => n.name.clone(),
                    _ => "call".to_string(),
                };
                let inputs = args.into_iter().map(|x| self.walk(x)).collect();
                let kind = if BOUNDARY_CALLS.contains(&callee_name.as_str()) {
                    NodeKind::Boundary
                } else {
                    // A pure call: a kernel, but not lowerable to scalar IR until
                    // we inline callee bodies. Opaque for now.
                    NodeKind::Kernel(KernelOp::Opaque)
                };
                self.emit(kind, format!("{callee_name}()"), span, inputs)
            }

            // Projection — selects a slot. Positional from FieldKey; named resolved
            // against the container's recorded field names.
            ExprKind::Index(ix) => {
                let (recv, idx_expr) = (ix.receiver, ix.index);
                let index = self.literal_int(idx_expr).unwrap_or(0);
                let container = self.walk(recv);
                self.emit(
                    NodeKind::Project { index },
                    format!("[{index}]"),
                    span,
                    vec![container],
                )
            }
            ExprKind::Field(fx) => {
                let recv = fx.receiver;
                let container = self.walk(recv);
                let (index, label) = match &fx.field {
                    FieldKey::Index(i) => (*i, format!("[{i}]")),
                    FieldKey::Name(name) => {
                        let idx = self
                            .field_names
                            .get(&container)
                            .and_then(|ns| ns.iter().position(|n| n == name))
                            .unwrap_or(0) as u32;
                        (idx, format!(".{name}"))
                    }
                };
                self.emit(NodeKind::Project { index }, label, span, vec![container])
            }

            // Control flow is kernel-internal (short-circuit). Both branches drawn
            // in the static snapshot; at runtime only one is taken.
            ExprKind::If(ifx) => {
                let (c, t, e2) = (ifx.condition, ifx.then_branch, ifx.else_branch);
                let mut inputs = vec![self.walk(c), self.walk(t)];
                if let Some(eb) = e2 {
                    inputs.push(self.walk(eb));
                }
                self.emit(NodeKind::Kernel(KernelOp::If), "if".into(), span, inputs)
            }

            // Explicit failure — poisons when reached. Its message is an input so
            // lowering can reconstruct it: a `fail e` carries `e`'s value to the
            // kernel through the value-pool side table (`lower::const_value` →
            // `Kernel::value_pool`), so a caught `?` gets the same typed `err` on the
            // kernel path as on the tree-walk.
            ExprKind::Fail(fx) => {
                let inputs = match fx.message {
                    Some(m) => vec![self.walk(m)],
                    None => vec![],
                };
                self.emit(NodeKind::Fail, "fail".into(), span, inputs)
            }

            // `exec [...] with { observer, mounts, ... }` — the build boundary.
            // References the underlying call and the keyword-config record.
            ExprKind::Where(w) => {
                let (callee, keywords) = (w.callee, w.keywords);
                let c = self.walk(callee);
                let k = self.walk(keywords);
                self.emit(NodeKind::Boundary, "where".into(), span, vec![c, k])
            }

            // A closure (observer / probe): a shipped kernel. Inline its body's
            // subgraph — captures resolve through the shared binding map, and its
            // own params are bound first. You see the observer's demand graph.
            ExprKind::Closure(cl) => {
                let body = cl.body;
                let cparams: Vec<PatternId> =
                    prog.arena[body].params.iter().map(|p| p.pattern).collect();
                for p in cparams {
                    self.bind_param(p);
                }
                let croot = prog.arena[body].root_expr;
                let child = self.walk(croot);
                self.emit(
                    NodeKind::Kernel(KernelOp::Opaque),
                    "(…)=>".into(),
                    span,
                    vec![child],
                )
            }

            // An interpolated string is a kernel over its embedded expressions.
            ExprKind::InterpolatedString(s) => {
                let exprs: Vec<ExprId> = s
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        StringPart::Expr(x) => Some(*x),
                        StringPart::Text(_) => None,
                    })
                    .collect();
                let inputs = exprs.into_iter().map(|x| self.walk(x)).collect();
                self.emit(
                    NodeKind::Kernel(KernelOp::Opaque),
                    "`…`".into(),
                    span,
                    inputs,
                )
            }

            // Bare invocation `f!` — a no-arg call.
            ExprKind::Invoke(iv) => {
                let name = match &prog.arena[iv.callee].kind {
                    ExprKind::NameRef(n) => n.name.clone(),
                    _ => "invoke".to_string(),
                };
                let kind = if BOUNDARY_CALLS.contains(&name.as_str()) {
                    NodeKind::Boundary
                } else {
                    NodeKind::Kernel(KernelOp::Opaque)
                };
                self.emit(kind, format!("{name}!"), span, vec![])
            }

            // A block: process its statements — bind `let`s (so names link to
            // their value subgraphs) and walk expression statements — then take the
            // tail, else the last expression statement, else unit, as the value.
            ExprKind::Block(bl) => {
                let stmts = bl.stmts.clone();
                let tail = bl.tail;
                let mut last_expr: Option<NodeId> = None;
                for s in stmts {
                    match &prog.arena[s].kind {
                        StmtKind::Let(l) => {
                            let (pat, init) = (l.pattern, l.init);
                            let node = self.walk(init);
                            if let PatternKind::Binding(pb) = &prog.arena[pat].kind {
                                self.bindings.insert(pb.binding, node);
                            }
                        }
                        StmtKind::Expr(e) => last_expr = Some(self.walk(*e)),
                        _ => {}
                    }
                }
                match tail.map(|t| self.walk(t)).or(last_expr) {
                    Some(v) => return v,
                    None => self.emit(NodeKind::Const, "()".into(), span, vec![]),
                }
            }

            // Path literal `p"a/{x}/b"` — a constant if pure, else a kernel over
            // its interpolated parts.
            ExprKind::PathLiteral(p) => {
                let exprs: Vec<ExprId> = p
                    .parts
                    .iter()
                    .filter_map(|pt| match pt {
                        StringPart::Expr(x) => Some(*x),
                        StringPart::Text(_) => None,
                    })
                    .collect();
                if exprs.is_empty() {
                    self.emit(NodeKind::Const, "path".into(), span, vec![])
                } else {
                    let inputs = exprs.into_iter().map(|x| self.walk(x)).collect();
                    self.emit(
                        NodeKind::Kernel(KernelOp::Opaque),
                        "p`…`".into(),
                        span,
                        inputs,
                    )
                }
            }

            // A map literal — lazy data over its entry values (and spreads).
            ExprKind::Map(m) => {
                let exprs: Vec<ExprId> = m
                    .entries
                    .iter()
                    .map(|el| match el {
                        MapElement::Entry(en) => en.value,
                        MapElement::Spread(x) => *x,
                    })
                    .collect();
                let inputs = exprs.into_iter().map(|x| self.walk(x)).collect();
                self.emit(NodeKind::LazyData, "%{…}".into(), span, inputs)
            }

            // Partial application `partial f(a, _)` — produces a kernel/closure.
            ExprKind::PartialApp(pa) => {
                let callee = self.walk(pa.callee);
                self.emit(
                    NodeKind::Kernel(KernelOp::Opaque),
                    "partial".into(),
                    span,
                    vec![callee],
                )
            }

            // `match scrutinee { pat => body, ... }` — control flow, kernel-internal
            // (short-circuit on the scrutinee). Static snapshot draws all arms.
            // Arm patterns that bind a simple name are bound best-effort.
            ExprKind::Match(mx) => {
                let scrutinee = mx.scrutinee;
                let arms: Vec<(PatternId, ExprId)> =
                    mx.arms.iter().map(|a| (a.pattern, a.body)).collect();
                let mut inputs = vec![self.walk(scrutinee)];
                for (pat, body) in arms {
                    self.bind_param(pat);
                    inputs.push(self.walk(body));
                }
                self.emit(
                    NodeKind::Kernel(KernelOp::Opaque),
                    "match".into(),
                    span,
                    inputs,
                )
            }

            // Range `a..b` / `a..=b` — a small constructed value over its bounds.
            ExprKind::Range(rx) => {
                let mut inputs = Vec::new();
                if let Some(s) = rx.start {
                    inputs.push(self.walk(s));
                }
                if let Some(en) = rx.end {
                    inputs.push(self.walk(en));
                }
                let label = if rx.inclusive { "..=" } else { ".." };
                self.emit(NodeKind::LazyData, label.into(), span, inputs)
            }

            // `expr?` — the fallible (Option-returning) variant; a kernel over expr.
            ExprKind::Fallible(fx) => {
                let inner = self.walk(fx.expr);
                self.emit(
                    NodeKind::Kernel(KernelOp::Opaque),
                    "?".into(),
                    span,
                    vec![inner],
                )
            }

            // A name: link to its bound value's node, else a leaf.
            ExprKind::NameRef(n) => {
                if let Some(DefTarget::Binding(bid)) = n.resolved
                    && let Some(&node) = self.bindings.get(&bid)
                {
                    return node;
                }
                self.emit(NodeKind::Param, n.name.clone(), span, vec![])
            }

            // Honest frontier: everything this snapshot doesn't model yet.
            other => self.emit(
                NodeKind::Unresolved,
                expr_kind_name(other).into(),
                span,
                vec![],
            ),
        };
        // For fall-through arms, `id` is the node just emitted for *this* expr —
        // stamp its real type. (Early-return arms forward to another node, which
        // already carries its own type.)
        self.nodes[id.0 as usize].ty = ty_str;
        self.memo.insert(e, id);
        id
    }

    /// Render a node's static type via the `TypeEnv` (`"?"` if not yet inferred).
    fn render_ty(&self, ty: Option<TypeId>) -> String {
        match ty {
            Some(tid) => format!("{}", self.env.get_ty(tid).display_with_env(self.env)),
            None => "?".to_string(),
        }
    }

    /// Emit a lazy-data node for a named record/struct and remember its field
    /// names (so `.field` projections resolve to the right slot).
    fn lazy_record(&mut self, label: String, fields: Vec<(String, ExprId)>, span: Span) -> NodeId {
        let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        let inputs = fields.into_iter().map(|(_, v)| self.walk(v)).collect();
        let id = self.emit(NodeKind::LazyData, label, span, inputs);
        self.field_names.insert(id, names);
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

/// Map a front-end [`BinOp`] to the graph's [`KernelOp`]. Scalar operators get a
/// [`BinKind`]; `++` (concat) and `|>` (pipe) aren't scalar, so they're
/// [`KernelOp::Opaque`] — the lowering will decline them, the evaluator handles
/// them. One total mapping, checked by the compiler, not a string.
fn bin_kind(op: BinOp) -> KernelOp {
    match op {
        BinOp::Add => KernelOp::Bin(BinKind::Add),
        BinOp::Sub => KernelOp::Bin(BinKind::Sub),
        BinOp::Mul => KernelOp::Bin(BinKind::Mul),
        BinOp::Div => KernelOp::Bin(BinKind::Div),
        BinOp::Rem => KernelOp::Bin(BinKind::Rem),
        BinOp::Eq => KernelOp::Bin(BinKind::Eq),
        BinOp::Neq => KernelOp::Bin(BinKind::Ne),
        BinOp::Lt => KernelOp::Bin(BinKind::Lt),
        BinOp::LtEq => KernelOp::Bin(BinKind::Le),
        BinOp::Gt => KernelOp::Bin(BinKind::Gt),
        BinOp::GtEq => KernelOp::Bin(BinKind::Ge),
        BinOp::And => KernelOp::Bin(BinKind::And),
        BinOp::Or => KernelOp::Bin(BinKind::Or),
        BinOp::Concat | BinOp::Pipe => KernelOp::Opaque,
    }
}

fn to_span(span: vx_parser::SourceSpan) -> Span {
    Span {
        start: span.start(),
        end: span.end(),
    }
}

fn expr_kind_name(k: &ExprKind) -> &'static str {
    match k {
        ExprKind::With(_) => "with",
        ExprKind::TypeOp(_) => "as/is",
        ExprKind::Import(_) => "import",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeKind;

    /// Every `Bomb` node in `g` must be unreached by demand.
    fn bombs_unreached(g: &DemandGraph) {
        let reached = g.demand_reachable();
        let bombs = g.nodes.iter().filter(|n| n.kind == NodeKind::Bomb).count();
        assert!(bombs >= 1, "expected at least one bomb, got: {:?}", g.nodes);
        for (i, n) in g.nodes.iter().enumerate() {
            if n.kind == NodeKind::Bomb {
                assert!(!reached[i], "bomb #{i} must be unreached by demand");
            }
        }
    }

    #[test]
    fn direct_tuple_projection() {
        bombs_unreached(&analyze_source("fn d() -> U64 ( (42, 💣)[0] )", "d").unwrap());
    }

    #[test]
    fn positional_index_selects_the_right_slot() {
        // bomb in slot 0; demanding [1] must not reach it.
        bombs_unreached(&analyze_source("fn d() -> U64 ( (💣, 11)[1] )", "d").unwrap());
    }

    #[test]
    fn let_binding_links_to_its_value() {
        // The canonical demand_basis shape: a let-bound tuple, then projection.
        // Only works because NameRef resolves through the binding to the tuple.
        let g = analyze_source("fn d() -> U64 ( let pair = (42, 💣); pair[0] )", "d").unwrap();
        bombs_unreached(&g);
    }

    #[test]
    fn named_struct_field_resolves_to_the_right_slot() {
        // Pair { a: 42, b: 💣 }.a — `.a` must resolve to slot 0, leaving b's bomb dark.
        let src = "struct Pair { a: U64, b: U64 } fn d() -> U64 ( Pair { a: 42, b: 💣 }.a )";
        bombs_unreached(&analyze_source(src, "d").unwrap());
    }

    #[test]
    fn nested_projection_reaches_the_leaf_not_the_bombs() {
        // The real demand_basis `deep_named_field_projection` shape:
        // Outer { a: Pair { a: 42, b: 💣 }, b: 💣 }; s.a.a  — demands the inner 42.
        // Catches the nested-projection bug: a naive container.inputs[index] would
        // leave the demanded 42 dark.
        let src = "struct Pair { a: U64, b: U64 } struct Outer { a: Pair, b: U64 } \
                   fn d() -> U64 ( let s = Outer { a: Pair { a: 42, b: 💣 }, b: 💣 }; s.a.a )";
        let g = analyze_source(src, "d").unwrap();
        bombs_unreached(&g);
        let reached = g.demand_reachable();
        let leaf_reached = g
            .nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.kind == NodeKind::Const && n.label == "42" && reached[i]);
        assert!(
            leaf_reached,
            "the demanded inner 42 (s.a.a) must be reached"
        );
    }

    #[test]
    fn call_args_are_walked() {
        // assert_eq(pair[0], 42): the call must reach into its args.
        let g = analyze_source("fn d() -> Bool ( let pair = (7, 💣); pair[0] == 7 )", "d").unwrap();
        bombs_unreached(&g);
    }

    #[test]
    fn explicit_fail_is_modelled_and_reached() {
        // poisons.vx `explicit_fail_detonates`: `fail "boom"`.
        let g = analyze_source(r#"fn d() -> U64 ( fail "boom" )"#, "d").unwrap();
        let reached = g.demand_reachable();
        let fail_reached = g
            .nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.kind == NodeKind::Fail && reached[i]);
        assert!(
            fail_reached,
            "`fail` must be a reached Fail node: {:?}",
            g.nodes
        );
    }

    #[test]
    fn a_demanded_bomb_is_reached() {
        // poisons.vx `forcing_a_bomb_detonates`: `(42, 💣)[1]` DEMANDS the bomb,
        // so it must be reached (it detonates — the point of a poisons test). The
        // mirror of demand_basis, and proof the walk isn't marking every bomb dark.
        let g = analyze_source("fn d() -> U64 ( let pair = (42, 💣); pair[1] )", "d").unwrap();
        let reached = g.demand_reachable();
        let bomb_reached = g
            .nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.kind == NodeKind::Bomb && reached[i]);
        assert!(bomb_reached, "the demanded bomb (pair[1]) must be reached");
    }
}
