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

use std::collections::{HashMap, HashSet};

use vx_ir::build::VirProgram;
use vx_ir::expr::{
    ArrayElement, BinOp, CliLiteralExpr, ExprKind, FieldKey, LitKind, MapElement, MapKey,
    PartialArg, PrefixOp, StringPart, UpdateField,
};
use vx_ir::ids::{BindingId, BodyId, ExprId, ItemId, PatternId};
use vx_ir::item::ItemKind;
use vx_ir::pattern::PatternKind;
use vx_ir::scope::DefTarget;
use vx_ir::stmt::StmtKind;
use vx_ir::type_ref::TypeRefKind;
use vx_types::{Ty, TypeDef, TypeEnv, TypeId};

use crate::graph::{
    BinKind, ConstValue, DemandGraph, HostMethod, KernelOp, MatchArm, MatchPattern, Node, NodeId,
    NodeKind, Span,
};

/// Intrinsic/boundary call names — effectful or non-deterministic, so the kernel
/// partition cuts here (a call to one of these is a `Boundary`, not a `Kernel`).
///
/// `discover` is deliberately **absent**: it is *deprecated* — host facts now come
/// from standing capability advertisement + predicate-based executor selection
/// (`docs/design/runners-and-control-plane.md` §6), not an inline `discover`.
/// `discover` is not a target; any remaining `discover(...)` call simply falls
/// through as an ordinary call here.
///
/// `exec` is likewise **absent**: running a tool is a backtick *invocation*
/// (`r[expr.command]`), not a call — its boundary is the `CliLiteral` arm below.
const BOUNDARY_CALLS: &[&str] = &["acquire", "Http", "http"];

#[derive(Debug, Clone, Default)]
pub(crate) struct GraphRuntimeMeta {
    pub(crate) execs: HashMap<NodeId, ExecInvocationMeta>,
    pub(crate) tree_files: HashMap<NodeId, TreeFileMeta>,
    pub(crate) capabilities: HashMap<NodeId, CapabilityRequestMeta>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecInvocationMeta {
    pub(crate) args: Vec<CliArgTemplate>,
    pub(crate) observer: Option<vx_ir::Bundle>,
    pub(crate) writable_roots: Vec<NodeId>,
    pub(crate) create_dirs: Vec<NodeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct CliArgTemplate {
    pub(crate) pieces: Vec<CliArgPiece>,
}

#[derive(Debug, Clone)]
pub(crate) enum CliArgPiece {
    Text(String),
    Node(NodeId),
}

#[derive(Debug, Clone)]
pub(crate) struct TreeFileMeta;

#[derive(Debug, Clone)]
pub(crate) struct CapabilityRequestMeta {
    pub(crate) spec: String,
    pub(crate) constraint: crate::ability::Constraint,
}

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
/// ([`crate::compile::semantic_front_end`]): parse → VIR → prelude → resolve →
/// type-check → capability-check, with packages + grammars loaded. vx-runtime is
/// the engine that owns this entry; vx-lsp and vx-diagnostics ride the same passes.
///
/// Strict: a `CompileError` (parse / resolve / type / capability) comes back as a
/// setup-error string — the demand evaluator only runs a well-formed program.
pub(crate) fn front_end(source: &str) -> Result<(VirProgram, TypeEnv), String> {
    let fe = crate::compile::semantic_front_end_bundled(source).map_err(|e| e.to_string())?;
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
    HashMap<BindingId, NodeId>,
) {
    let (graph, expr_nodes, param_bindings, binding_nodes, _) =
        build_graph_indexed_with_meta(program, env, body_id);
    (graph, expr_nodes, param_bindings, binding_nodes)
}

pub(crate) fn build_graph_indexed_with_meta(
    program: &VirProgram,
    env: &TypeEnv,
    body_id: BodyId,
) -> (
    DemandGraph,
    HashMap<ExprId, NodeId>,
    HashMap<NodeId, BindingId>,
    HashMap<BindingId, NodeId>,
    GraphRuntimeMeta,
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
        param_bindings: HashMap::new(),
        field_names: HashMap::new(),
        active_bodies: vec![body_id],
        callables: HashMap::new(),
        runtime_meta: GraphRuntimeMeta::default(),
    };
    for pat in params {
        b.bind_pattern(pat);
    }
    let root = b.walk(root_expr);
    let graph = DemandGraph {
        nodes: b.nodes,
        root,
    };
    (graph, b.memo, b.param_bindings, b.bindings, b.runtime_meta)
}

struct Builder<'a> {
    program: &'a VirProgram,
    env: &'a TypeEnv,
    nodes: Vec<Node>,
    memo: HashMap<ExprId, NodeId>,
    /// Local binding → the node of its value (the link that makes `pair[0]` reach
    /// the construction). Flat (shadowing not modelled yet).
    bindings: HashMap<BindingId, NodeId>,
    /// Nodes introduced by patterns/function parameters and backed by runtime env
    /// entries. This deliberately excludes let bindings whose value node happens to
    /// be `NodeKind::Param` (for example a bare unit variant name).
    param_bindings: HashMap<NodeId, BindingId>,
    /// For a lazy-data node built from a named struct/record: its field names in
    /// slot order, so a named projection (`.a`) can resolve to the right slot.
    field_names: HashMap<NodeId, Vec<String>>,
    /// Function bodies currently being expanded into the graph. Direct calls are
    /// graph-expanded at analysis time until graph templates exist; this prevents
    /// recursive expansion from rebuilding the source tree forever.
    active_bodies: Vec<BodyId>,
    /// Analyzer-local callable templates keyed by their graph value node. The
    /// executable graph does not store VIR ids; this table is used only while
    /// expanding call sites into graph nodes.
    callables: HashMap<NodeId, CallableTemplate>,
    runtime_meta: GraphRuntimeMeta,
}

#[derive(Clone)]
struct CallableTemplate {
    body: BodyId,
    captures: HashMap<BindingId, NodeId>,
    code_identity: Option<String>,
}

#[derive(Default)]
struct ExecKeywords {
    observer: Option<BodyId>,
    writable_roots: Vec<NodeId>,
    create_dirs: Vec<NodeId>,
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

    /// Bind every name introduced by a pattern to a fresh `Param` node, so name refs
    /// in the matched body link to the value supplied by runtime pattern matching.
    fn bind_pattern(&mut self, pat: PatternId) {
        let prog = self.program;
        let p = &prog.arena[pat];
        match &p.kind {
            PatternKind::Binding(pb) => {
                let (name, binding) = (pb.name.clone(), pb.binding);
                let span = to_span(p.span);
                let node = self.emit(NodeKind::Param, name, span, vec![]);
                self.bindings.insert(binding, node);
                self.param_bindings.insert(node, binding);
            }
            PatternKind::Tuple(tp) => {
                for &child in &tp.elements {
                    self.bind_pattern(child);
                }
            }
            PatternKind::Struct(sp) => {
                for field in &sp.fields {
                    self.bind_pattern(field.pattern);
                }
            }
            PatternKind::Array(ap) => {
                for &child in &ap.elements {
                    self.bind_pattern(child);
                }
            }
            PatternKind::ArrayRest(rest) => {
                if let Some(binding) = rest.binding {
                    let name = rest.name.clone().unwrap_or_else(|| "..".to_string());
                    let span = to_span(p.span);
                    let node = self.emit(NodeKind::Param, name, span, vec![]);
                    self.bindings.insert(binding, node);
                    self.param_bindings.insert(node, binding);
                }
            }
            PatternKind::Variant(vp) => {
                if let Some(fields) = &vp.fields {
                    for field in fields {
                        self.bind_pattern(field.pattern);
                    }
                }
            }
            PatternKind::At(at) => {
                let span = to_span(p.span);
                let node = self.emit(NodeKind::Param, at.name.clone(), span, vec![]);
                self.bindings.insert(at.binding, node);
                self.param_bindings.insert(node, at.binding);
                self.bind_pattern(at.inner);
            }
            PatternKind::Or(or) => {
                for &child in &or.alternatives {
                    self.bind_pattern(child);
                }
            }
            PatternKind::Wildcard
            | PatternKind::Literal(_)
            | PatternKind::Unit
            | PatternKind::Error => {}
        }
    }

    fn direct_function_template(&self, callee: ExprId) -> Option<CallableTemplate> {
        let ExprKind::NameRef(name) = &self.program.arena[callee].kind else {
            return None;
        };
        let Some(DefTarget::Item(item)) = name.resolved else {
            return None;
        };
        match &self.program.arena[item].kind {
            ItemKind::Function(function) => Some(CallableTemplate {
                body: function.body,
                captures: HashMap::new(),
                code_identity: Some(function_code_identity(
                    self.program,
                    &name.name,
                    function.body,
                )),
            }),
            _ => None,
        }
    }

    fn emit_callable_value(
        &mut self,
        body: BodyId,
        label: String,
        span: Span,
        captures: HashMap<BindingId, NodeId>,
        code_identity: Option<String>,
    ) -> NodeId {
        let inputs = sorted_capture_nodes(&captures);
        let node = self.emit(NodeKind::Closure, label, span, inputs);
        self.callables.insert(
            node,
            CallableTemplate {
                body,
                captures,
                code_identity,
            },
        );
        node
    }

    fn captures_for_body(&self, body: BodyId) -> HashMap<BindingId, NodeId> {
        self.program.arena[body]
            .captures
            .iter()
            .filter_map(|binding| self.bindings.get(binding).map(|&node| (*binding, node)))
            .collect()
    }

    fn emit_exec_invocation(
        &mut self,
        cli: &CliLiteralExpr,
        meta: ExecKeywords,
        span: Span,
    ) -> NodeId {
        let args = self.cli_arg_templates(cli);
        let mut inputs: Vec<NodeId> = args
            .iter()
            .flat_map(|arg| arg.pieces.iter())
            .filter_map(|piece| match piece {
                CliArgPiece::Text(_) => None,
                CliArgPiece::Node(node) => Some(*node),
            })
            .collect();
        inputs.extend(meta.writable_roots.iter().copied());
        inputs.extend(meta.create_dirs.iter().copied());
        let node = self.emit(NodeKind::Boundary, "`…`".into(), span, inputs);
        let observer = meta
            .observer
            .map(|body| vx_ir::reachable(&self.program.arena, self.env, body));
        self.runtime_meta.execs.insert(
            node,
            ExecInvocationMeta {
                args,
                observer,
                writable_roots: meta.writable_roots,
                create_dirs: meta.create_dirs,
            },
        );
        node
    }

    fn cli_arg_templates(&mut self, cli: &CliLiteralExpr) -> Vec<CliArgTemplate> {
        cli.args
            .iter()
            .map(|arg| CliArgTemplate {
                pieces: arg
                    .pieces
                    .iter()
                    .map(|piece| match piece {
                        StringPart::Text(text) => CliArgPiece::Text(text.clone()),
                        StringPart::Expr(expr) => CliArgPiece::Node(self.walk(*expr)),
                    })
                    .collect(),
            })
            .collect()
    }

    fn invocation_keywords(&mut self, keywords: ExprId) -> Result<ExecKeywords, String> {
        let ExprKind::Record(record) = &self.program.arena[keywords].kind else {
            return Err("an invocation's `with` clause must be a keyword record".into());
        };
        let observer = match record.fields.iter().find(|field| field.name == "observer") {
            Some(field) => match &self.program.arena[field.value].kind {
                ExprKind::Closure(closure) => Some(closure.body),
                _ => return Err("an invocation's `observer` must be a closure".into()),
            },
            None => None,
        };
        let writable_roots = self.keyword_path_array(record, "writable")?;
        let create_dirs = self.keyword_path_array(record, "create_dirs")?;
        Ok(ExecKeywords {
            observer,
            writable_roots,
            create_dirs,
        })
    }

    fn keyword_path_array(
        &mut self,
        record: &vx_ir::expr::RecordExpr,
        name: &str,
    ) -> Result<Vec<NodeId>, String> {
        let Some(field) = record.fields.iter().find(|field| field.name == name) else {
            return Ok(Vec::new());
        };
        let ExprKind::Array(array) = &self.program.arena[field.value].kind else {
            return Err(format!(
                "an invocation's `{name}` keyword must be an array literal in graph exec"
            ));
        };
        let elements = array.elements.clone();
        let mut roots = Vec::with_capacity(elements.len());
        for element in elements {
            match element {
                ArrayElement::Expr(expr) => roots.push(self.walk(expr)),
                ArrayElement::Spread(_) => {
                    return Err(format!(
                        "an invocation's `{name}` keyword does not support spreads in graph exec yet"
                    ));
                }
            }
        }
        Ok(roots)
    }

    fn capability_request(&mut self, w: &vx_ir::expr::WhereExpr, span: Span) -> NodeId {
        let ExprKind::NameRef(name) = &self.program.arena[w.callee].kind else {
            let callee = self.walk(w.callee);
            return self.emit(
                NodeKind::Unresolved,
                "a `where` constraint must apply to a capability spec".into(),
                span,
                vec![callee],
            );
        };
        match crate::ability::extract(&self.program.arena, w.keywords) {
            Ok(constraint) => {
                let spec = name.name.clone();
                let node = self.emit(NodeKind::Boundary, format!("{spec} where"), span, vec![]);
                self.runtime_meta
                    .capabilities
                    .insert(node, CapabilityRequestMeta { spec, constraint });
                node
            }
            Err(reason) => {
                let c = self.walk(w.callee);
                let k = self.walk(w.keywords);
                self.emit(NodeKind::Unresolved, reason, span, vec![c, k])
            }
        }
    }

    fn method_kernel_call(
        &mut self,
        callee: ExprId,
        arg_exprs: &[ExprId],
        span: Span,
    ) -> Option<NodeId> {
        let ExprKind::Field(field) = &self.program.arena[callee].kind else {
            return None;
        };
        let FieldKey::Name(name) = &field.field else {
            return None;
        };
        match name.as_str() {
            "len" if arg_exprs.is_empty() => {
                let receiver = self.walk(field.receiver);
                Some(self.emit(
                    NodeKind::Kernel(KernelOp::StrLen),
                    ".len()".into(),
                    span,
                    vec![receiver],
                ))
            }
            "which" => {
                let mut inputs = Vec::with_capacity(1 + arg_exprs.len());
                inputs.push(self.walk(field.receiver));
                inputs.extend(arg_exprs.iter().map(|&arg| self.walk(arg)));
                Some(self.emit(
                    NodeKind::HostMethod {
                        method: HostMethod::Which,
                    },
                    ".which()".into(),
                    span,
                    inputs,
                ))
            }
            "exec" => {
                let mut inputs = Vec::with_capacity(1 + arg_exprs.len());
                inputs.push(self.walk(field.receiver));
                inputs.extend(arg_exprs.iter().map(|&arg| self.walk(arg)));
                Some(self.emit(
                    NodeKind::HostMethod {
                        method: HostMethod::Exec,
                    },
                    ".exec()".into(),
                    span,
                    inputs,
                ))
            }
            "file" => {
                let mut inputs = Vec::with_capacity(1 + arg_exprs.len());
                inputs.push(self.walk(field.receiver));
                inputs.extend(arg_exprs.iter().map(|&arg| self.walk(arg)));
                let node = self.emit(NodeKind::Boundary, ".file()".into(), span, inputs);
                self.runtime_meta.tree_files.insert(node, TreeFileMeta);
                Some(node)
            }
            _ => None,
        }
    }

    fn variant_call(&mut self, callee: ExprId, arg_exprs: &[ExprId], span: Span) -> Option<NodeId> {
        let ExprKind::NameRef(name) = &self.program.arena[callee].kind else {
            return None;
        };
        let Some(DefTarget::Variant {
            enum_item,
            variant_index,
        }) = name.resolved
        else {
            return None;
        };
        let tag = self.variant_name(enum_item, variant_index, &name.name);
        let fields = arg_exprs
            .iter()
            .enumerate()
            .map(|(index, &expr)| (index.to_string(), expr))
            .collect();
        Some(self.variant_value(tag, fields, span))
    }

    fn callable_template(&mut self, callee: ExprId) -> Option<CallableTemplate> {
        let node = self.walk(callee);
        self.callables.get(&node).cloned()
    }

    fn expand_callable(
        &mut self,
        template: CallableTemplate,
        args: &[NodeId],
        span: Span,
        callee_name: &str,
    ) -> NodeId {
        let body = template.body;
        if self.active_bodies.contains(&body) {
            return self.emit(
                NodeKind::Unresolved,
                format!("recursive call `{callee_name}`"),
                span,
                args.to_vec(),
            );
        }

        let prog = self.program;
        let params: Vec<PatternId> = prog.arena[body]
            .params
            .iter()
            .map(|param| param.pattern)
            .collect();
        if params.len() != args.len() {
            return self.emit(
                NodeKind::Unresolved,
                format!(
                    "call `{callee_name}` expected {} args, got {}",
                    params.len(),
                    args.len()
                ),
                span,
                args.to_vec(),
            );
        }

        let root_expr = prog.arena[body].root_expr;
        let saved_bindings = self.bindings.clone();
        let saved_param_bindings = self.param_bindings.clone();
        let saved_memo = self.memo.clone();

        let code_identity = template.code_identity.clone();
        let capture_inputs = sorted_capture_nodes(&template.captures);
        let capture_count = capture_inputs.len() as u32;

        self.active_bodies.push(body);
        for (binding, node) in template.captures {
            self.bindings.insert(binding, node);
        }
        for (param, arg) in params.into_iter().zip(args.iter().copied()) {
            self.bind_pattern_to_node(param, arg);
        }
        let root = self.walk(root_expr);
        self.active_bodies.pop();

        self.bindings = saved_bindings;
        self.param_bindings = saved_param_bindings;
        self.memo = saved_memo;

        let mut inputs = Vec::with_capacity(args.len() + capture_inputs.len() + 1);
        inputs.push(root);
        inputs.extend_from_slice(args);
        inputs.extend_from_slice(&capture_inputs);
        self.emit(
            NodeKind::Call {
                function: callee_name.to_string(),
                code_identity,
                arg_count: args.len() as u32,
                capture_count,
            },
            format!("{callee_name}(…)"),
            span,
            inputs,
        )
    }

    fn bind_pattern_to_node(&mut self, pat: PatternId, node: NodeId) {
        let prog = self.program;
        let p = &prog.arena[pat];
        match &p.kind {
            PatternKind::Binding(pb) => {
                self.bindings.insert(pb.binding, node);
            }
            PatternKind::Tuple(tp) => {
                for (index, &child) in tp.elements.iter().enumerate() {
                    let field = self.emit(
                        NodeKind::Project {
                            index: index as u32,
                        },
                        format!("[{index}]"),
                        to_span(p.span),
                        vec![node],
                    );
                    self.bind_pattern_to_node(child, field);
                }
            }
            PatternKind::Struct(sp) => {
                for (fallback, field) in sp.fields.iter().enumerate() {
                    let index = self
                        .field_index_by_node(node, &field.name)
                        .unwrap_or(fallback) as u32;
                    let field_node = self.emit(
                        NodeKind::Project { index },
                        format!(".{}", field.name),
                        to_span(field.name_span),
                        vec![node],
                    );
                    self.bind_pattern_to_node(field.pattern, field_node);
                }
            }
            PatternKind::Array(ap) => {
                for (index, &child) in ap.elements.iter().enumerate() {
                    let field = self.emit(
                        NodeKind::Project {
                            index: index as u32,
                        },
                        format!("[{index}]"),
                        to_span(p.span),
                        vec![node],
                    );
                    self.bind_pattern_to_node(child, field);
                }
            }
            PatternKind::ArrayRest(rest) => {
                if let Some(binding) = rest.binding {
                    self.bindings.insert(binding, node);
                }
            }
            PatternKind::Variant(vp) => {
                if let Some(fields) = &vp.fields {
                    for (fallback, field) in fields.iter().enumerate() {
                        let field_node = self.emit(
                            NodeKind::Project {
                                index: fallback as u32,
                            },
                            format!(".{}", field.name),
                            to_span(field.name_span),
                            vec![node],
                        );
                        self.bind_pattern_to_node(field.pattern, field_node);
                    }
                }
            }
            PatternKind::At(at) => {
                self.bindings.insert(at.binding, node);
                self.bind_pattern_to_node(at.inner, node);
            }
            PatternKind::Or(or) => {
                for &child in &or.alternatives {
                    self.bind_pattern_to_node(child, node);
                }
            }
            PatternKind::Wildcard
            | PatternKind::Literal(_)
            | PatternKind::Unit
            | PatternKind::Error => {}
        }
    }

    fn bind_match_pattern(&mut self, pat: PatternId, node: NodeId) {
        let prog = self.program;
        let p = &prog.arena[pat];
        match &p.kind {
            PatternKind::Binding(pb) => {
                self.bindings.insert(pb.binding, node);
            }
            PatternKind::At(at) => {
                self.bindings.insert(at.binding, node);
                self.bind_match_pattern(at.inner, node);
            }
            PatternKind::Variant(vp) => {
                if let Some(fields) = &vp.fields {
                    for (fallback, field) in fields.iter().enumerate() {
                        let field_node = self.emit(
                            NodeKind::Project {
                                index: fallback as u32,
                            },
                            format!(".{}", field.name),
                            to_span(field.name_span),
                            vec![node],
                        );
                        self.bind_match_pattern(field.pattern, field_node);
                    }
                }
            }
            PatternKind::Tuple(_)
            | PatternKind::Struct(_)
            | PatternKind::Array(_)
            | PatternKind::ArrayRest(_)
            | PatternKind::Or(_) => {}
            PatternKind::Wildcard
            | PatternKind::Literal(_)
            | PatternKind::Unit
            | PatternKind::Error => {}
        }
    }

    fn match_pattern(&self, pat: PatternId) -> MatchPattern {
        let p = &self.program.arena[pat];
        match &p.kind {
            PatternKind::Binding(_) => MatchPattern::Binding,
            PatternKind::Wildcard => MatchPattern::Wildcard,
            PatternKind::Literal(lit) => {
                let (value, _) = const_value_and_label(&lit.value);
                MatchPattern::Const(value)
            }
            PatternKind::Unit => MatchPattern::Const(ConstValue::Unit),
            PatternKind::Variant(vp) => MatchPattern::Variant {
                tag: vp.name.clone(),
            },
            other => MatchPattern::Unsupported(format!("{other:?}")),
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
                let (value, label) = const_value_and_label(&lit.value);
                self.emit(NodeKind::Const(value), label, span, vec![])
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
                if s.resolved_variant.is_some() {
                    let tag = s.variant.clone().unwrap_or(label);
                    self.variant_value(tag, pairs, span)
                } else {
                    self.lazy_record(label, pairs, span)
                }
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

            // A call: assertions are pure kernel nodes, known effects are boundary
            // nodes, and direct function calls are expanded into graph nodes with
            // argument node refs bound to callee params.
            ExprKind::Call(c) => {
                let callee = c.callee;
                let callee_name = match &prog.arena[callee].kind {
                    ExprKind::NameRef(n) => n.name.clone(),
                    _ => "call".to_string(),
                };
                let arg_exprs: Vec<ExprId> = c.args.iter().map(|a| a.expr).collect();

                if let Some(variant_node) = self.variant_call(callee, &arg_exprs, span) {
                    self.memo.insert(e, variant_node);
                    return variant_node;
                }

                if let Some(method_node) = self.method_kernel_call(callee, &arg_exprs, span) {
                    self.memo.insert(e, method_node);
                    return method_node;
                }

                match callee_name.as_str() {
                    "assert" => {
                        let inputs = arg_exprs.into_iter().map(|x| self.walk(x)).collect();
                        self.emit(
                            NodeKind::Kernel(KernelOp::Assert),
                            format!("{callee_name}()"),
                            span,
                            inputs,
                        )
                    }
                    "assert_eq" => {
                        let inputs = arg_exprs.into_iter().map(|x| self.walk(x)).collect();
                        self.emit(
                            NodeKind::Kernel(KernelOp::AssertEq),
                            format!("{callee_name}()"),
                            span,
                            inputs,
                        )
                    }
                    name if BOUNDARY_CALLS.contains(&name) => {
                        let inputs = arg_exprs.into_iter().map(|x| self.walk(x)).collect();
                        self.emit(NodeKind::Boundary, format!("{callee_name}()"), span, inputs)
                    }
                    _ => {
                        if let Some(template) = self.direct_function_template(callee) {
                            let args: Vec<NodeId> =
                                arg_exprs.into_iter().map(|x| self.walk(x)).collect();
                            let expanded =
                                self.expand_callable(template, &args, span, &callee_name);
                            self.memo.insert(e, expanded);
                            return expanded;
                        }

                        if let Some(template) = self.callable_template(callee) {
                            let args: Vec<NodeId> =
                                arg_exprs.into_iter().map(|x| self.walk(x)).collect();
                            let expanded =
                                self.expand_callable(template, &args, span, &callee_name);
                            self.memo.insert(e, expanded);
                            return expanded;
                        }

                        let inputs = arg_exprs.into_iter().map(|x| self.walk(x)).collect();
                        self.emit(
                            NodeKind::Unresolved,
                            format!("unsupported call `{callee_name}`"),
                            span,
                            inputs,
                        )
                    }
                }
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
                            .field_index_by_node(container, name)
                            .or_else(|| self.field_index_by_receiver_type(recv, name))
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
            // `Kernel::value_pool`), so a caught `?` gets the same typed `err` when
            // the graph path lowers this region into a kernel.
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
                if w.is_constraint {
                    self.capability_request(w, span)
                } else if !w.is_constraint
                    && let ExprKind::CliLiteral(cli) = &prog.arena[w.callee].kind
                {
                    match self.invocation_keywords(w.keywords) {
                        Ok(keywords) => self.emit_exec_invocation(cli, keywords, span),
                        Err(reason) => {
                            let c = self.walk(w.callee);
                            let k = self.walk(w.keywords);
                            self.emit(NodeKind::Unresolved, reason, span, vec![c, k])
                        }
                    }
                } else {
                    let (callee, keywords) = (w.callee, w.keywords);
                    let c = self.walk(callee);
                    let k = self.walk(keywords);
                    self.emit(NodeKind::Boundary, "where".into(), span, vec![c, k])
                }
            }

            // A closure value: captures are node refs, not demanded values. The
            // body is expanded only when a call site demands the callable.
            ExprKind::Closure(cl) => {
                let body = cl.body;
                let captures = self.captures_for_body(body);
                self.emit_callable_value(
                    body,
                    "(…)=>".into(),
                    span,
                    captures,
                    Some(closure_code_identity(self.program, body)),
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
                    "\"…\"".into(),
                    span,
                    inputs,
                )
            }

            // A backtick command invocation (`r[expr.command]`): forcing it runs an
            // external process, so it is a Boundary. Its inputs are the holes — the
            // leading `$tool` and each `${arg}` — so the graph reaches them.
            ExprKind::CliLiteral(cli) => self.emit_exec_invocation(cli, ExecKeywords::default(), span),

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
                    Some(v) => {
                        self.memo.insert(e, v);
                        return v;
                    }
                    None => self.emit(NodeKind::Const(ConstValue::Unit), "()".into(), span, vec![]),
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

            // `import "@vixen/grammar-cc"` as a graph value: a lazy record with one
            // field per grammar coordinate in the package.
            ExprKind::Import(imp) => match prog.arena.grammars.get(&imp.path) {
                Some(set) => self.grammar_package_value(&imp.path, set, span),
                None => self.emit(
                    NodeKind::Unresolved,
                    format!(
                        "import `{}` is not a grammar package (only grammar imports are values yet)",
                        imp.path
                    ),
                    span,
                    vec![],
                ),
            },

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

            // `match scrutinee { pat => body, ... }`: demand the scrutinee, then
            // only the selected arm body. Binding patterns alias the scrutinee's
            // node inside that arm.
            ExprKind::Match(mx) => {
                let scrutinee = mx.scrutinee;
                let arms: Vec<(PatternId, ExprId)> =
                    mx.arms.iter().map(|a| (a.pattern, a.body)).collect();
                let scrutinee_node = self.walk(scrutinee);
                let mut match_arms = Vec::with_capacity(arms.len());
                for (pat, body) in arms {
                    let saved_bindings = self.bindings.clone();
                    let saved_param_bindings = self.param_bindings.clone();
                    let saved_memo = self.memo.clone();
                    self.bind_match_pattern(pat, scrutinee_node);
                    let body_node = self.walk(body);
                    self.bindings = saved_bindings;
                    self.param_bindings = saved_param_bindings;
                    self.memo = saved_memo;
                    match_arms.push(MatchArm {
                        pattern: self.match_pattern(pat),
                        body: body_node,
                    });
                }
                self.emit(
                    NodeKind::Match { arms: match_arms },
                    "match".into(),
                    span,
                    vec![scrutinee_node],
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

            // `expr?` — a graph-level catch. The fallible node demands its inner in
            // catch mode; if that demand poisons, the driver memoizes the paired
            // caught-failure node and the fallible node returns `Err(Failure { .. })`.
            ExprKind::Fallible(fx) => {
                let inner = self.walk(fx.expr);
                let fallible = self.emit(
                    NodeKind::Fallible {
                        caught_failure: NodeId(u32::MAX),
                    },
                    "?".into(),
                    span,
                    vec![inner],
                );
                let failure = self.emit(
                    NodeKind::CaughtFailure,
                    "Failure".into(),
                    span,
                    vec![fallible],
                );
                self.nodes[fallible.0 as usize].kind = NodeKind::Fallible {
                    caught_failure: failure,
                };
                fallible
            }

            // A coercion is a transparent single-child wrapper: forward to its
            // child's node so demand flows through it unchanged (identity at
            // runtime for `String -> Path`).
            ExprKind::Coerce(c) => {
                let node = self.walk(c.expr);
                self.memo.insert(e, node);
                return node;
            }

            // A name: link to its bound value's node. A body/frame parameter is
            // introduced by `bind_pattern`; resolved non-local names are values,
            // not kernel parameters.
            ExprKind::NameRef(n) => {
                if let Some(DefTarget::Binding(bid)) = n.resolved
                    && let Some(&node) = self.bindings.get(&bid)
                {
                    self.memo.insert(e, node);
                    return node;
                }
                match n.resolved {
                    Some(DefTarget::Variant {
                        enum_item,
                        variant_index,
                    }) => {
                        let tag = self.variant_name(enum_item, variant_index, &n.name);
                        self.emit(
                            NodeKind::Variant {
                                tag: tag.clone(),
                                field_names: Vec::new(),
                            },
                            tag,
                            span,
                            vec![],
                        )
                    }
                    Some(DefTarget::Item(item)) => match &prog.arena[item].kind {
                        ItemKind::Function(function) => self.emit_callable_value(
                            function.body,
                            n.name.clone(),
                            span,
                            HashMap::new(),
                            Some(function_code_identity(prog, &n.name, function.body)),
                        ),
                        _ => self.emit(NodeKind::Unresolved, n.name.clone(), span, vec![]),
                    },
                    Some(DefTarget::Binding(_)) | None => {
                        self.emit(NodeKind::Unresolved, n.name.clone(), span, vec![])
                    }
                }
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
        let inputs: Vec<NodeId> = fields.into_iter().map(|(_, v)| self.walk(v)).collect();
        self.lazy_record_nodes(label, names.into_iter().zip(inputs).collect(), span)
    }

    fn lazy_record_nodes(
        &mut self,
        label: String,
        fields: Vec<(String, NodeId)>,
        span: Span,
    ) -> NodeId {
        let names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
        let inputs = fields.into_iter().map(|(_, node)| node).collect();
        let id = self.emit(NodeKind::LazyData, label, span, inputs);
        self.field_names.insert(id, names);
        id
    }

    fn grammar_package_value(
        &mut self,
        package: &str,
        set: &command_model::GrammarSet,
        span: Span,
    ) -> NodeId {
        let fields = set
            .grammars
            .iter()
            .map(|grammar| {
                (
                    grammar.name.clone(),
                    self.grammar_coord_value(package, &grammar.name, span),
                )
            })
            .collect();
        self.lazy_record_nodes(format!("import {package:?}"), fields, span)
    }

    fn grammar_coord_value(&mut self, package: &str, name: &str, span: Span) -> NodeId {
        let package = self.emit(
            NodeKind::Const(ConstValue::Str(package.to_string())),
            package.to_string(),
            span,
            vec![],
        );
        let name = self.emit(
            NodeKind::Const(ConstValue::Str(name.to_string())),
            name.to_string(),
            span,
            vec![],
        );
        self.lazy_record_nodes(
            "grammar".into(),
            vec![("package".into(), package), ("name".into(), name)],
            span,
        )
    }

    fn variant_value(&mut self, tag: String, fields: Vec<(String, ExprId)>, span: Span) -> NodeId {
        let field_names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
        let inputs = fields.into_iter().map(|(_, v)| self.walk(v)).collect();
        self.emit(
            NodeKind::Variant {
                tag: tag.clone(),
                field_names,
            },
            tag,
            span,
            inputs,
        )
    }

    fn variant_name(&self, enum_item: ItemId, variant_index: u32, fallback: &str) -> String {
        match &self.program.arena[enum_item].kind {
            ItemKind::Enum(e) => e
                .variants
                .get(variant_index as usize)
                .map(|variant| variant.name.clone())
                .unwrap_or_else(|| fallback.to_string()),
            _ => fallback.to_string(),
        }
    }

    fn field_index_by_receiver_type(&self, receiver: ExprId, name: &str) -> Option<usize> {
        let ty = self.program.arena[receiver].ty?;
        let Ty::Struct(struct_ty) = self.env.get_ty(ty) else {
            return None;
        };
        let TypeDef::Struct(struct_def) = self.env.get_def(struct_ty.def) else {
            return None;
        };
        struct_def
            .fields
            .iter()
            .position(|field| field.name == name)
    }

    fn field_index_by_node(&self, node: NodeId, name: &str) -> Option<usize> {
        let resolved = self.resolved_graph_node(node).unwrap_or(node);
        self.field_names
            .get(&resolved)
            .and_then(|names| names.iter().position(|field| field == name))
    }

    fn resolved_graph_node(&self, node: NodeId) -> Option<NodeId> {
        let graph_node = self.nodes.get(node.0 as usize)?;
        match &graph_node.kind {
            NodeKind::Project { index } => {
                let raw_container = *graph_node.inputs.first()?;
                let container = self
                    .resolved_graph_node(raw_container)
                    .unwrap_or(raw_container);
                let container_node = self.nodes.get(container.0 as usize)?;
                match &container_node.kind {
                    NodeKind::LazyData => {
                        let slot = *container_node.inputs.get(*index as usize)?;
                        Some(self.resolved_graph_node(slot).unwrap_or(slot))
                    }
                    _ => None,
                }
            }
            NodeKind::Call { .. } => {
                let body_root = *graph_node.inputs.first()?;
                Some(self.resolved_graph_node(body_root).unwrap_or(body_root))
            }
            _ => Some(node),
        }
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

fn sorted_capture_nodes(captures: &HashMap<BindingId, NodeId>) -> Vec<NodeId> {
    let mut captures: Vec<(BindingId, NodeId)> = captures
        .iter()
        .map(|(&binding, &node)| (binding, node))
        .collect();
    captures.sort_by_key(|(binding, _)| *binding);
    captures.into_iter().map(|(_, node)| node).collect()
}

fn function_code_identity(program: &VirProgram, _name: &str, body: BodyId) -> String {
    body_code_hash(program, body)
}

fn closure_code_identity(program: &VirProgram, body: BodyId) -> String {
    let material = ClosureCodeIdentityMaterial {
        schema: "vixen.closure-code-identity.v1".into(),
        parent_body_hash: program.arena[body]
            .parent
            .map(|parent| body_local_code_hash(program, parent)),
        closure_body_hash: body_code_hash(program, body),
    };
    format!("closure:{}", hash_identity_material(&material))
}

fn body_code_hash(program: &VirProgram, body: BodyId) -> String {
    let mut graph = FunctionCodeIdentityGraph::new(program);
    graph.body_code_hash(body)
}

struct FunctionCodeIdentityGraph<'a> {
    program: &'a VirProgram,
    functions: HashMap<BodyId, String>,
    edges: HashMap<BodyId, Vec<(String, BodyId)>>,
    sccs: Vec<Vec<BodyId>>,
    body_to_scc: HashMap<BodyId, usize>,
    scc_hashes: HashMap<usize, String>,
}

impl<'a> FunctionCodeIdentityGraph<'a> {
    fn new(program: &'a VirProgram) -> Self {
        let mut functions = HashMap::new();
        for item in &program.arena.items {
            if let ItemKind::Function(function) = &item.kind {
                functions.insert(function.body, function.name.clone());
            }
        }

        let edges: HashMap<BodyId, Vec<(String, BodyId)>> = functions
            .keys()
            .copied()
            .map(|body| {
                let refs = function_refs_in_body_and_child_closures(program, body)
                    .into_iter()
                    .filter(|(_, callee)| functions.contains_key(callee))
                    .collect();
                (body, refs)
            })
            .collect();
        let sccs = strongly_connected_function_bodies(&functions, &edges);
        let body_to_scc = sccs
            .iter()
            .enumerate()
            .flat_map(|(scc, bodies)| bodies.iter().map(move |&body| (body, scc)))
            .collect();

        Self {
            program,
            functions,
            edges,
            sccs,
            body_to_scc,
            scc_hashes: HashMap::new(),
        }
    }

    fn body_code_hash(&mut self, body: BodyId) -> String {
        if let Some(&scc) = self.body_to_scc.get(&body) {
            return self.scc_code_hash(scc);
        }
        let mut active = Vec::new();
        self.non_scc_body_code_hash(body, &mut active)
    }

    fn non_scc_body_code_hash(&mut self, body: BodyId, active: &mut Vec<BodyId>) -> String {
        if active.contains(&body) {
            return body_local_code_hash(self.program, body);
        }

        active.push(body);
        let function_refs = function_refs_in_body_and_child_closures(self.program, body)
            .into_iter()
            .map(|(name, body)| FunctionRefIdentity {
                name,
                body_hash: self.body_code_hash(body),
            })
            .collect();
        active.pop();

        let material = TransitiveCodeIdentityMaterial {
            schema: "vixen.body-code-identity.v3".into(),
            local_body_hash: body_local_code_hash(self.program, body),
            function_refs,
        };
        hash_identity_material(&material)
    }

    fn scc_code_hash(&mut self, scc: usize) -> String {
        if let Some(hash) = self.scc_hashes.get(&scc) {
            return hash.clone();
        }

        let members = self.sccs[scc].clone();
        let mut member_identities: Vec<SccMemberIdentity> = members
            .iter()
            .map(|&body| SccMemberIdentity {
                name: self.functions[&body].clone(),
                local_body_hash: body_local_code_hash(self.program, body),
            })
            .collect();
        member_identities.sort_by(|left, right| left.name.cmp(&right.name));

        let mut function_refs = Vec::new();
        for &body in &members {
            let refs = self.edges.get(&body).cloned().unwrap_or_default();
            for (name, callee) in refs {
                let Some(&dep_scc) = self.body_to_scc.get(&callee) else {
                    continue;
                };
                if dep_scc == scc {
                    continue;
                }
                function_refs.push(FunctionRefIdentity {
                    name,
                    body_hash: self.scc_code_hash(dep_scc),
                });
            }
        }
        function_refs.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.body_hash.cmp(&right.body_hash))
        });
        function_refs
            .dedup_by(|left, right| left.name == right.name && left.body_hash == right.body_hash);

        let material = SccCodeIdentityMaterial {
            schema: "vixen.function-scc-code-identity.v1".into(),
            members: member_identities,
            function_refs,
        };
        let hash = hash_identity_material(&material);
        self.scc_hashes.insert(scc, hash.clone());
        hash
    }
}

fn body_local_code_hash(program: &VirProgram, body: BodyId) -> String {
    let material = CodeIdentityMaterial {
        schema: "vixen.body-local-code-identity.v1".into(),
        body: body_identity_node(program, body),
    };
    hash_identity_material(&material)
}

fn hash_identity_material<T: for<'facet> facet::Facet<'facet>>(material: &T) -> String {
    let json = facet_json::to_string(material).expect("code identity material is serializable");
    format!("blake3:{}", blake3::hash(json.as_bytes()).to_hex())
}

fn function_refs_in_body_and_child_closures(
    program: &VirProgram,
    body: BodyId,
) -> Vec<(String, BodyId)> {
    let mut bodies = vec![body];
    let mut cursor = 0;
    while cursor < bodies.len() {
        let parent = bodies[cursor];
        for (child, _) in program.arena.child_bodies(parent) {
            bodies.push(child);
        }
        cursor += 1;
    }

    let mut refs: Vec<(String, BodyId)> = program
        .arena
        .exprs
        .iter()
        .filter(|expr| bodies.contains(&expr.body))
        .filter_map(|expr| {
            let ExprKind::NameRef(name) = &expr.kind else {
                return None;
            };
            let Some(DefTarget::Item(item)) = name.resolved else {
                return None;
            };
            let ItemKind::Function(function) = &program.arena[item].kind else {
                return None;
            };
            Some((name.name.clone(), function.body))
        })
        .collect();
    refs.sort();
    refs.dedup();
    refs
}

fn strongly_connected_function_bodies(
    functions: &HashMap<BodyId, String>,
    edges: &HashMap<BodyId, Vec<(String, BodyId)>>,
) -> Vec<Vec<BodyId>> {
    struct Tarjan<'a> {
        edges: &'a HashMap<BodyId, Vec<(String, BodyId)>>,
        index: usize,
        stack: Vec<BodyId>,
        on_stack: HashSet<BodyId>,
        indices: HashMap<BodyId, usize>,
        lowlinks: HashMap<BodyId, usize>,
        sccs: Vec<Vec<BodyId>>,
    }

    impl Tarjan<'_> {
        fn strong_connect(&mut self, body: BodyId) {
            let index = self.index;
            self.index += 1;
            self.indices.insert(body, index);
            self.lowlinks.insert(body, index);
            self.stack.push(body);
            self.on_stack.insert(body);

            for (_, callee) in self.edges.get(&body).into_iter().flatten() {
                if !self.indices.contains_key(callee) {
                    self.strong_connect(*callee);
                    let callee_lowlink = self.lowlinks[callee];
                    let body_lowlink = self.lowlinks[&body];
                    self.lowlinks.insert(body, body_lowlink.min(callee_lowlink));
                } else if self.on_stack.contains(callee) {
                    let callee_index = self.indices[callee];
                    let body_lowlink = self.lowlinks[&body];
                    self.lowlinks.insert(body, body_lowlink.min(callee_index));
                }
            }

            if self.lowlinks[&body] == self.indices[&body] {
                let mut scc = Vec::new();
                loop {
                    let member = self.stack.pop().expect("tarjan stack member");
                    self.on_stack.remove(&member);
                    scc.push(member);
                    if member == body {
                        break;
                    }
                }
                scc.sort();
                self.sccs.push(scc);
            }
        }
    }

    let mut bodies: Vec<BodyId> = functions.keys().copied().collect();
    bodies.sort_by(|left, right| {
        functions[left]
            .cmp(&functions[right])
            .then_with(|| left.cmp(right))
    });

    let mut tarjan = Tarjan {
        edges,
        index: 0,
        stack: Vec::new(),
        on_stack: HashSet::new(),
        indices: HashMap::new(),
        lowlinks: HashMap::new(),
        sccs: Vec::new(),
    };
    for body in bodies {
        if !tarjan.indices.contains_key(&body) {
            tarjan.strong_connect(body);
        }
    }
    tarjan.sccs
}

#[derive(facet::Facet, Debug)]
struct CodeIdentityMaterial {
    schema: String,
    body: IdentityNode,
}

#[derive(facet::Facet, Debug)]
struct TransitiveCodeIdentityMaterial {
    schema: String,
    local_body_hash: String,
    function_refs: Vec<FunctionRefIdentity>,
}

#[derive(facet::Facet, Debug)]
struct FunctionRefIdentity {
    name: String,
    body_hash: String,
}

#[derive(facet::Facet, Debug)]
struct SccCodeIdentityMaterial {
    schema: String,
    members: Vec<SccMemberIdentity>,
    function_refs: Vec<FunctionRefIdentity>,
}

#[derive(facet::Facet, Debug)]
struct SccMemberIdentity {
    name: String,
    local_body_hash: String,
}

#[derive(facet::Facet, Debug)]
struct ClosureCodeIdentityMaterial {
    schema: String,
    parent_body_hash: Option<String>,
    closure_body_hash: String,
}

#[derive(facet::Facet, Debug)]
struct IdentityNode {
    tag: String,
    data: Vec<IdentityField>,
    children: Vec<IdentityNode>,
}

#[derive(facet::Facet, Debug)]
struct IdentityField {
    name: String,
    value: String,
}

fn inode(
    tag: impl Into<String>,
    data: Vec<IdentityField>,
    children: Vec<IdentityNode>,
) -> IdentityNode {
    IdentityNode {
        tag: tag.into(),
        data,
        children,
    }
}

fn idata(name: impl Into<String>, value: impl Into<String>) -> IdentityField {
    IdentityField {
        name: name.into(),
        value: value.into(),
    }
}

fn body_identity_node(program: &VirProgram, body: BodyId) -> IdentityNode {
    let body = &program.arena[body];
    let mut children: Vec<IdentityNode> = body
        .params
        .iter()
        .map(|param| {
            let mut children = vec![pattern_identity_node(program, param.pattern)];
            if let Some(ty) = param.ty {
                children.push(type_ref_identity_node(program, ty));
            }
            inode(
                "param",
                vec![
                    idata("keyword", param.is_keyword.to_string()),
                    idata("has_default", param.has_default.to_string()),
                ],
                children,
            )
        })
        .collect();
    if let Some(ret) = body.return_ty {
        children.push(inode(
            "return_ty",
            Vec::new(),
            vec![type_ref_identity_node(program, ret)],
        ));
    }
    children.push(inode(
        "root",
        Vec::new(),
        vec![expr_identity_node(program, body.root_expr)],
    ));
    inode("body", Vec::new(), children)
}

fn expr_identity_node(program: &VirProgram, expr: ExprId) -> IdentityNode {
    match &program.arena[expr].kind {
        ExprKind::Literal(lit) => inode("literal", lit_identity_data(&lit.value), Vec::new()),
        ExprKind::NameRef(name) => inode(
            "name_ref",
            vec![
                idata("name", name.name.clone()),
                idata(
                    "resolved",
                    def_target_identity(program, name.resolved).unwrap_or_else(|| "none".into()),
                ),
            ],
            Vec::new(),
        ),
        ExprKind::Block(block) => {
            let mut children: Vec<IdentityNode> = block
                .stmts
                .iter()
                .map(|&stmt| stmt_identity_node(program, stmt))
                .collect();
            children.push(optional_expr_identity(program, "tail", block.tail));
            inode(
                "block",
                vec![idata("synthetic", block.synthetic.to_string())],
                children,
            )
        }
        ExprKind::Closure(closure) => inode(
            "closure",
            Vec::new(),
            vec![body_identity_node(program, closure.body)],
        ),
        ExprKind::Tuple(tuple) => inode(
            "tuple",
            Vec::new(),
            tuple
                .elements
                .iter()
                .map(|&expr| expr_identity_node(program, expr))
                .collect(),
        ),
        ExprKind::Array(array) => inode(
            "array",
            Vec::new(),
            array
                .elements
                .iter()
                .map(|element| match element {
                    ArrayElement::Expr(expr) => inode(
                        "array_expr",
                        Vec::new(),
                        vec![expr_identity_node(program, *expr)],
                    ),
                    ArrayElement::Spread(expr) => inode(
                        "array_spread",
                        Vec::new(),
                        vec![expr_identity_node(program, *expr)],
                    ),
                })
                .collect(),
        ),
        ExprKind::Record(record) => inode(
            "record",
            Vec::new(),
            record
                .fields
                .iter()
                .map(|field| {
                    inode(
                        "record_field",
                        vec![idata("name", field.name.clone())],
                        vec![expr_identity_node(program, field.value)],
                    )
                })
                .collect(),
        ),
        ExprKind::Map(map) => inode(
            "map",
            Vec::new(),
            map.entries
                .iter()
                .map(|element| match element {
                    MapElement::Entry(entry) => {
                        let mut children = vec![map_key_identity_node(program, &entry.key)];
                        children.push(expr_identity_node(program, entry.value));
                        inode("map_entry", Vec::new(), children)
                    }
                    MapElement::Spread(expr) => inode(
                        "map_spread",
                        Vec::new(),
                        vec![expr_identity_node(program, *expr)],
                    ),
                })
                .collect(),
        ),
        ExprKind::StructLiteral(strukt) => inode(
            "struct_literal",
            vec![
                idata("name", strukt.name.clone()),
                idata("variant", strukt.variant.clone().unwrap_or_default()),
                idata(
                    "resolved",
                    strukt
                        .resolved
                        .map(|item| program.arena.item_name(item).to_string())
                        .unwrap_or_else(|| "none".into()),
                ),
            ],
            strukt
                .fields
                .iter()
                .map(|field| {
                    inode(
                        "struct_field",
                        vec![idata("name", field.name.clone())],
                        vec![expr_identity_node(program, field.value)],
                    )
                })
                .collect(),
        ),
        ExprKind::Call(call) => {
            let mut children = vec![inode(
                "callee",
                Vec::new(),
                vec![expr_identity_node(program, call.callee)],
            )];
            children.extend(call.args.iter().map(|arg| {
                inode(
                    "arg",
                    Vec::new(),
                    vec![expr_identity_node(program, arg.expr)],
                )
            }));
            children.extend(call.type_args.iter().map(|&ty| {
                inode(
                    "type_arg",
                    Vec::new(),
                    vec![type_ref_identity_node(program, ty)],
                )
            }));
            inode("call", Vec::new(), children)
        }
        ExprKind::Where(w) => inode(
            "where",
            vec![idata("constraint", w.is_constraint.to_string())],
            vec![
                expr_identity_node(program, w.callee),
                expr_identity_node(program, w.keywords),
            ],
        ),
        ExprKind::Invoke(invoke) => inode(
            "invoke",
            Vec::new(),
            vec![expr_identity_node(program, invoke.callee)],
        ),
        ExprKind::PartialApp(partial) => {
            let mut children = vec![inode(
                "callee",
                Vec::new(),
                vec![expr_identity_node(program, partial.callee)],
            )];
            children.extend(partial.args.iter().map(|arg| match arg {
                PartialArg::Expr(expr) => inode(
                    "partial_expr",
                    Vec::new(),
                    vec![expr_identity_node(program, *expr)],
                ),
                PartialArg::Placeholder { .. } => {
                    inode("partial_placeholder", Vec::new(), Vec::new())
                }
            }));
            inode("partial_app", Vec::new(), children)
        }
        ExprKind::Index(index) => inode(
            "index",
            Vec::new(),
            vec![
                expr_identity_node(program, index.receiver),
                expr_identity_node(program, index.index),
            ],
        ),
        ExprKind::Field(field) => inode(
            "field",
            vec![idata("field", field_key_identity(&field.field))],
            vec![expr_identity_node(program, field.receiver)],
        ),
        ExprKind::If(if_expr) => inode(
            "if",
            Vec::new(),
            vec![
                inode(
                    "condition",
                    Vec::new(),
                    vec![expr_identity_node(program, if_expr.condition)],
                ),
                inode(
                    "then",
                    Vec::new(),
                    vec![expr_identity_node(program, if_expr.then_branch)],
                ),
                optional_expr_identity(program, "else", if_expr.else_branch),
            ],
        ),
        ExprKind::Match(match_expr) => {
            let mut children = vec![inode(
                "scrutinee",
                Vec::new(),
                vec![expr_identity_node(program, match_expr.scrutinee)],
            )];
            children.extend(match_expr.arms.iter().map(|arm| {
                inode(
                    "arm",
                    Vec::new(),
                    vec![
                        pattern_identity_node(program, arm.pattern),
                        expr_identity_node(program, arm.body),
                    ],
                )
            }));
            inode("match", Vec::new(), children)
        }
        ExprKind::Binary(binary) => inode(
            "binary",
            vec![idata("op", format!("{:?}", binary.op))],
            vec![
                expr_identity_node(program, binary.lhs),
                expr_identity_node(program, binary.rhs),
            ],
        ),
        ExprKind::Prefix(prefix) => inode(
            "prefix",
            vec![idata("op", format!("{:?}", prefix.op))],
            vec![expr_identity_node(program, prefix.operand)],
        ),
        ExprKind::Range(range) => inode(
            "range",
            vec![idata("inclusive", range.inclusive.to_string())],
            vec![
                optional_expr_identity(program, "start", range.start),
                optional_expr_identity(program, "end", range.end),
            ],
        ),
        ExprKind::TypeOp(type_op) => inode(
            "type_op",
            vec![idata("op", format!("{:?}", type_op.op))],
            vec![
                expr_identity_node(program, type_op.expr),
                type_ref_identity_node(program, type_op.ty),
            ],
        ),
        ExprKind::With(with) => {
            let mut children = vec![inode(
                "receiver",
                Vec::new(),
                vec![expr_identity_node(program, with.receiver)],
            )];
            children.extend(
                with.updates
                    .iter()
                    .map(|update| update_field_identity_node(program, update)),
            );
            inode("with", Vec::new(), children)
        }
        ExprKind::Fallible(fallible) => inode(
            "fallible",
            Vec::new(),
            vec![expr_identity_node(program, fallible.expr)],
        ),
        ExprKind::InterpolatedString(s) => inode(
            "interpolated_string",
            Vec::new(),
            s.parts
                .iter()
                .map(|part| string_part_identity_node(program, part))
                .collect(),
        ),
        ExprKind::Import(import) => inode(
            "import",
            vec![
                idata("path", import.path.clone()),
                idata("resolved", import.resolved_source.is_some().to_string()),
            ],
            Vec::new(),
        ),
        ExprKind::CliLiteral(cli) => inode(
            "cli_literal",
            vec![idata("grammar", cli.grammar.clone())],
            cli.args
                .iter()
                .map(|arg| {
                    inode(
                        "cli_arg",
                        Vec::new(),
                        arg.pieces
                            .iter()
                            .map(|part| string_part_identity_node(program, part))
                            .collect(),
                    )
                })
                .collect(),
        ),
        ExprKind::Fail(fail) => inode(
            "fail",
            Vec::new(),
            vec![optional_expr_identity(program, "message", fail.message)],
        ),
        ExprKind::Coerce(coerce) => inode(
            "coerce",
            vec![idata(
                "function",
                program.arena.item_name(coerce.coerce_fn).to_string(),
            )],
            vec![expr_identity_node(program, coerce.expr)],
        ),
        ExprKind::Bomb => inode("bomb", Vec::new(), Vec::new()),
        ExprKind::Error => inode("error", Vec::new(), Vec::new()),
    }
}

fn stmt_identity_node(program: &VirProgram, stmt: vx_ir::ids::StmtId) -> IdentityNode {
    match &program.arena[stmt].kind {
        StmtKind::Let(let_stmt) => {
            let mut children = vec![pattern_identity_node(program, let_stmt.pattern)];
            if let Some(ty) = let_stmt.ty {
                children.push(type_ref_identity_node(program, ty));
            }
            children.push(expr_identity_node(program, let_stmt.init));
            inode("let", Vec::new(), children)
        }
        StmtKind::Assign(assign) => inode(
            "assign",
            vec![
                idata("name", assign.name.clone()),
                idata(
                    "resolved",
                    assign
                        .resolved
                        .map(|binding| program.arena[binding].name.clone())
                        .unwrap_or_else(|| "none".into()),
                ),
            ],
            vec![expr_identity_node(program, assign.value)],
        ),
        StmtKind::CompoundUpdate(update) => inode(
            "compound_update",
            vec![
                idata("name", update.name.clone()),
                idata(
                    "resolved",
                    update
                        .resolved
                        .map(|binding| program.arena[binding].name.clone())
                        .unwrap_or_else(|| "none".into()),
                ),
            ],
            update
                .updates
                .iter()
                .map(|field| update_field_identity_node(program, field))
                .collect(),
        ),
        StmtKind::FieldAssign(assign) => inode(
            "field_assign",
            vec![
                idata("base", assign.base.clone()),
                idata("path", assign.field_path.join(".")),
                idata(
                    "resolved",
                    assign
                        .resolved
                        .map(|binding| program.arena[binding].name.clone())
                        .unwrap_or_else(|| "none".into()),
                ),
            ],
            vec![expr_identity_node(program, assign.value)],
        ),
        StmtKind::Return(ret) => inode(
            "return",
            Vec::new(),
            vec![optional_expr_identity(program, "value", ret.value)],
        ),
        StmtKind::Item(item) => inode(
            "item_stmt",
            Vec::new(),
            vec![item_identity_node(program, *item)],
        ),
        StmtKind::Expr(expr) => inode(
            "expr_stmt",
            Vec::new(),
            vec![expr_identity_node(program, *expr)],
        ),
        StmtKind::Error => inode("stmt_error", Vec::new(), Vec::new()),
    }
}

fn pattern_identity_node(program: &VirProgram, pattern: PatternId) -> IdentityNode {
    match &program.arena[pattern].kind {
        PatternKind::Binding(binding) => inode(
            "binding_pattern",
            vec![idata("name", binding.name.clone())],
            Vec::new(),
        ),
        PatternKind::Wildcard => inode("wildcard_pattern", Vec::new(), Vec::new()),
        PatternKind::Literal(lit) => {
            inode("literal_pattern", lit_identity_data(&lit.value), Vec::new())
        }
        PatternKind::Tuple(tuple) => inode(
            "tuple_pattern",
            Vec::new(),
            tuple
                .elements
                .iter()
                .map(|&pat| pattern_identity_node(program, pat))
                .collect(),
        ),
        PatternKind::Struct(strukt) => inode(
            "struct_pattern",
            Vec::new(),
            strukt
                .fields
                .iter()
                .map(|field| {
                    inode(
                        "struct_pattern_field",
                        vec![idata("name", field.name.clone())],
                        vec![pattern_identity_node(program, field.pattern)],
                    )
                })
                .collect(),
        ),
        PatternKind::Array(array) => inode(
            "array_pattern",
            Vec::new(),
            array
                .elements
                .iter()
                .map(|&pat| pattern_identity_node(program, pat))
                .collect(),
        ),
        PatternKind::ArrayRest(rest) => inode(
            "array_rest_pattern",
            vec![idata("name", rest.name.clone().unwrap_or_default())],
            Vec::new(),
        ),
        PatternKind::Unit => inode("unit_pattern", Vec::new(), Vec::new()),
        PatternKind::Variant(variant) => inode(
            "variant_pattern",
            vec![
                idata("name", variant.name.clone()),
                idata(
                    "resolved",
                    variant
                        .resolved
                        .map(|item| program.arena.item_name(item).to_string())
                        .unwrap_or_else(|| "none".into()),
                ),
            ],
            variant
                .fields
                .as_ref()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|field| {
                            inode(
                                "variant_pattern_field",
                                vec![idata("name", field.name.clone())],
                                vec![pattern_identity_node(program, field.pattern)],
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        PatternKind::At(at) => inode(
            "at_pattern",
            vec![idata("name", at.name.clone())],
            vec![pattern_identity_node(program, at.inner)],
        ),
        PatternKind::Or(or) => inode(
            "or_pattern",
            Vec::new(),
            or.alternatives
                .iter()
                .map(|&pat| pattern_identity_node(program, pat))
                .collect(),
        ),
        PatternKind::Error => inode("pattern_error", Vec::new(), Vec::new()),
    }
}

fn item_identity_node(program: &VirProgram, item: ItemId) -> IdentityNode {
    match &program.arena[item].kind {
        ItemKind::Function(function) => inode(
            "function_item",
            vec![
                idata("name", function.name.clone()),
                idata("type_params", function.type_params.join(",")),
            ],
            vec![
                body_identity_node(program, function.body),
                attributes_identity_node(program, &function.attributes),
            ],
        ),
        ItemKind::Struct(strukt) => inode(
            "struct_item",
            vec![
                idata("name", strukt.name.clone()),
                idata("type_params", strukt.type_params.join(",")),
            ],
            strukt
                .fields
                .iter()
                .map(|field| field_decl_identity_node(program, field))
                .chain(std::iter::once(attributes_identity_node(
                    program,
                    &strukt.attributes,
                )))
                .collect(),
        ),
        ItemKind::Enum(enum_item) => inode(
            "enum_item",
            vec![
                idata("name", enum_item.name.clone()),
                idata("type_params", enum_item.type_params.join(",")),
            ],
            enum_item
                .variants
                .iter()
                .map(|variant| {
                    inode(
                        "variant_decl",
                        vec![idata("name", variant.name.clone())],
                        variant
                            .fields
                            .iter()
                            .map(|field| field_decl_identity_node(program, field))
                            .chain(std::iter::once(attributes_identity_node(
                                program,
                                &variant.attributes,
                            )))
                            .collect(),
                    )
                })
                .chain(std::iter::once(attributes_identity_node(
                    program,
                    &enum_item.attributes,
                )))
                .collect(),
        ),
        ItemKind::Newtype(newtype) => {
            let mut children = Vec::new();
            if let Some(ty) = newtype.inner_ty {
                children.push(type_ref_identity_node(program, ty));
            }
            children.push(attributes_identity_node(program, &newtype.attributes));
            inode(
                "newtype_item",
                vec![
                    idata("name", newtype.name.clone()),
                    idata("type_params", newtype.type_params.join(",")),
                ],
                children,
            )
        }
        ItemKind::TypeAlias(alias) => {
            let mut children = Vec::new();
            if let Some(ty) = alias.aliased_ty {
                children.push(type_ref_identity_node(program, ty));
            }
            children.push(attributes_identity_node(program, &alias.attributes));
            inode(
                "type_alias_item",
                vec![
                    idata("name", alias.name.clone()),
                    idata("type_params", alias.type_params.join(",")),
                ],
                children,
            )
        }
        ItemKind::Namespace(namespace) => inode(
            "namespace_item",
            vec![
                idata("name", namespace.name.clone()),
                idata("type_params", namespace.type_params.join(",")),
            ],
            namespace
                .items
                .iter()
                .map(|&item| item_identity_node(program, item))
                .chain(std::iter::once(attributes_identity_node(
                    program,
                    &namespace.attributes,
                )))
                .collect(),
        ),
        ItemKind::Error => inode("item_error", Vec::new(), Vec::new()),
    }
}

fn field_decl_identity_node(program: &VirProgram, field: &vx_ir::item::FieldDecl) -> IdentityNode {
    let mut children = Vec::new();
    if let Some(ty) = field.ty {
        children.push(type_ref_identity_node(program, ty));
    }
    if let Some(default) = field.default_value {
        children.push(expr_identity_node(program, default));
    }
    children.push(attributes_identity_node(program, &field.attributes));
    inode(
        "field_decl",
        vec![
            idata("name", field.name.clone()),
            idata("has_default", field.has_default.to_string()),
        ],
        children,
    )
}

fn attributes_identity_node(
    program: &VirProgram,
    attrs: &[vx_ir::item::Attribute],
) -> IdentityNode {
    inode(
        "attributes",
        Vec::new(),
        attrs
            .iter()
            .map(|attr| {
                inode(
                    "attribute",
                    Vec::new(),
                    attr.entries
                        .iter()
                        .map(|entry| {
                            inode(
                                "attribute_entry",
                                vec![idata("key", entry.key.clone())],
                                entry
                                    .value
                                    .map(|value| vec![expr_identity_node(program, value)])
                                    .unwrap_or_default(),
                            )
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

fn type_ref_identity_node(program: &VirProgram, ty: vx_ir::ids::TypeRefId) -> IdentityNode {
    match &program.arena[ty].kind {
        TypeRefKind::Path(path) => inode(
            "type_path",
            vec![idata("segments", path.segments.join("."))],
            path.type_args
                .iter()
                .map(|&ty| type_ref_identity_node(program, ty))
                .collect(),
        ),
        TypeRefKind::Function(function) => {
            let mut children: Vec<IdentityNode> = function
                .params
                .iter()
                .map(|&ty| type_ref_identity_node(program, ty))
                .collect();
            children.push(inode(
                "return",
                Vec::new(),
                vec![type_ref_identity_node(program, function.ret)],
            ));
            inode("type_function", Vec::new(), children)
        }
        TypeRefKind::Tuple(tuple) => inode(
            "type_tuple",
            Vec::new(),
            tuple
                .elements
                .iter()
                .map(|&ty| type_ref_identity_node(program, ty))
                .collect(),
        ),
        TypeRefKind::Sum(sum) => inode(
            "type_sum",
            Vec::new(),
            sum.alternatives
                .iter()
                .map(|&ty| type_ref_identity_node(program, ty))
                .collect(),
        ),
        TypeRefKind::Error => inode("type_error", Vec::new(), Vec::new()),
    }
}

fn update_field_identity_node(program: &VirProgram, field: &UpdateField) -> IdentityNode {
    match field {
        UpdateField::Set { path, value, .. } => inode(
            "update_set",
            vec![idata("path", path.join("."))],
            vec![expr_identity_node(program, *value)],
        ),
        UpdateField::Nested { path, updates, .. } => inode(
            "update_nested",
            vec![idata("path", path.join("."))],
            updates
                .iter()
                .map(|update| update_field_identity_node(program, update))
                .collect(),
        ),
    }
}

fn string_part_identity_node(program: &VirProgram, part: &StringPart) -> IdentityNode {
    match part {
        StringPart::Text(text) => {
            inode("string_text", vec![idata("text", text.clone())], Vec::new())
        }
        StringPart::Expr(expr) => inode(
            "string_expr",
            Vec::new(),
            vec![expr_identity_node(program, *expr)],
        ),
    }
}

fn map_key_identity_node(program: &VirProgram, key: &MapKey) -> IdentityNode {
    match key {
        MapKey::Expr(expr) => inode(
            "map_key_expr",
            Vec::new(),
            vec![expr_identity_node(program, *expr)],
        ),
        MapKey::Name { name, .. } => inode(
            "map_key_name",
            vec![idata("name", name.clone())],
            Vec::new(),
        ),
    }
}

fn optional_expr_identity(
    program: &VirProgram,
    tag: impl Into<String>,
    expr: Option<ExprId>,
) -> IdentityNode {
    match expr {
        Some(expr) => inode(
            tag,
            vec![idata("present", "true")],
            vec![expr_identity_node(program, expr)],
        ),
        None => inode(tag, vec![idata("present", "false")], Vec::new()),
    }
}

fn lit_identity_data(lit: &LitKind) -> Vec<IdentityField> {
    match lit {
        LitKind::Int { raw } => vec![idata("kind", "int"), idata("raw", raw.clone())],
        LitKind::Float { raw } => vec![idata("kind", "float"), idata("raw", raw.clone())],
        LitKind::String { raw } => vec![idata("kind", "string"), idata("raw", raw.clone())],
        LitKind::Bool(value) => vec![idata("kind", "bool"), idata("value", value.to_string())],
    }
}

fn field_key_identity(field: &FieldKey) -> String {
    match field {
        FieldKey::Name(name) => name.clone(),
        FieldKey::Index(index) => index.to_string(),
    }
}

fn def_target_identity(program: &VirProgram, target: Option<DefTarget>) -> Option<String> {
    target.map(|target| match target {
        DefTarget::Binding(binding) => format!("binding:{}", program.arena[binding].name.as_str()),
        DefTarget::Item(item) => format!("item:{}", program.arena.item_name(item)),
        DefTarget::Variant {
            enum_item,
            variant_index,
        } => format!(
            "variant:{}:{}",
            program.arena.item_name(enum_item),
            variant_index
        ),
    })
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

fn const_value_and_label(lit: &LitKind) -> (ConstValue, String) {
    match lit {
        LitKind::Int { raw } => (
            raw.parse::<u64>()
                .map(ConstValue::U64)
                .unwrap_or_else(|_| ConstValue::Unsupported(raw.clone())),
            raw.clone(),
        ),
        LitKind::Float { raw } => (ConstValue::Float(raw.clone()), raw.clone()),
        LitKind::String { raw } => (ConstValue::Str(raw.clone()), format!("{raw:?}")),
        LitKind::Bool(b) => (ConstValue::Bool(*b), b.to_string()),
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

    struct CallIdentity<'a> {
        value: &'a str,
        arg_count: u32,
        capture_count: u32,
    }

    fn call_code_identity<'a>(g: &'a DemandGraph, name: &str) -> Option<CallIdentity<'a>> {
        g.nodes.iter().find_map(|node| match &node.kind {
            NodeKind::Call {
                function,
                code_identity: Some(code_identity),
                arg_count,
                capture_count,
            } if function == name => Some(CallIdentity {
                value: code_identity.as_str(),
                arg_count: *arg_count,
                capture_count: *capture_count,
            }),
            _ => None,
        })
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
    fn exec_invocation_metadata_carries_writable_roots_and_create_dirs() {
        let src = "fn t(cmd: Command) -> Tree ( \
                   `$cmd` with { \
                     writable: [\"out\"], \
                     create_dirs: [\"out/obj\"], \
                     observer: (run) => run.ok.out \
                   } )";
        let fe = crate::compile::compile_source(src, "t").expect("program compiles");
        let entry = fe
            .program
            .arena
            .items
            .iter()
            .find_map(|item| match &item.kind {
                ItemKind::Function(function) if function.name == "t" => Some(function.body),
                _ => None,
            })
            .expect("entry body");
        let (graph, _, _, _, meta) = build_graph_indexed_with_meta(&fe.program, &fe.env, entry);
        let exec = meta
            .execs
            .values()
            .next()
            .expect("one exec boundary is recorded");
        assert_eq!(
            exec.writable_roots.len(),
            1,
            "the writable root is part of exec runtime metadata"
        );
        let root = exec.writable_roots[0];
        let node = &graph.nodes[root.0 as usize];
        assert!(
            matches!(&node.kind, NodeKind::Const(ConstValue::Str(value)) if value == "out"),
            "writable root node should be the literal path, got {node:?}"
        );
        assert_eq!(
            exec.create_dirs.len(),
            1,
            "create_dirs is part of exec runtime metadata"
        );
        let dir = exec.create_dirs[0];
        let node = &graph.nodes[dir.0 as usize];
        assert!(
            matches!(&node.kind, NodeKind::Const(ConstValue::Str(value)) if value == "out/obj"),
            "create dir node should be the literal path, got {node:?}"
        );
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
            .any(|(i, n)| matches!(n.kind, NodeKind::Const(ConstValue::U64(42))) && reached[i]);
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
    fn direct_function_call_leaves_a_graph_call_node() {
        let g = analyze_source("fn id(x: U64) -> U64 ( x ) fn d() -> U64 ( id(42) )", "d").unwrap();
        let code_identity = call_code_identity(&g, "id").expect("id call identity");
        assert_eq!(code_identity.arg_count, 1);
        assert_eq!(code_identity.capture_count, 0);
        assert!(
            code_identity.value.starts_with("blake3:"),
            "direct calls must leave a source-blind Call node for runtime memo hooks: {:?}",
            g.nodes
        );
    }

    #[test]
    fn direct_function_call_identity_changes_with_body_content() {
        let first =
            analyze_source("fn id(x: U64) -> U64 ( x ) fn d() -> U64 ( id(42) )", "d").unwrap();
        let second = analyze_source(
            "fn id(x: U64) -> U64 ( x + 0 ) fn d() -> U64 ( id(42) )",
            "d",
        )
        .unwrap();
        let first_identity = call_code_identity(&first, "id").expect("first id identity");
        let second_identity = call_code_identity(&second, "id").expect("second id identity");
        assert_ne!(
            first_identity.value, second_identity.value,
            "code identity must change when the function body changes"
        );
    }

    #[test]
    fn direct_function_call_identity_ignores_formatting() {
        let compact =
            analyze_source("fn id(x: U64) -> U64 ( x ) fn d() -> U64 ( id(42) )", "d").unwrap();
        let spaced = analyze_source(
            "fn id(x: U64) -> U64 (\n    x\n)\n\nfn d() -> U64 (\n    id(42)\n)",
            "d",
        )
        .unwrap();
        let compact_identity = call_code_identity(&compact, "id").expect("compact id identity");
        let spaced_identity = call_code_identity(&spaced, "id").expect("spaced id identity");
        assert_eq!(
            compact_identity.value, spaced_identity.value,
            "code identity material must not include source spans"
        );
    }

    #[test]
    fn direct_function_call_identity_changes_with_callee_body_content() {
        let first = analyze_source(
            "fn helper() -> U64 ( 1 ) fn id(x: U64) -> U64 ( helper() + x ) fn d() -> U64 ( id(41) )",
            "d",
        )
        .unwrap();
        let second = analyze_source(
            "fn helper() -> U64 ( 2 ) fn id(x: U64) -> U64 ( helper() + x ) fn d() -> U64 ( id(41) )",
            "d",
        )
        .unwrap();
        let first_identity = call_code_identity(&first, "id").expect("first id identity");
        let second_identity = call_code_identity(&second, "id").expect("second id identity");
        assert_ne!(
            first_identity.value, second_identity.value,
            "caller identity must include referenced function body identity"
        );
    }

    #[test]
    fn direct_function_call_identity_ignores_unreferenced_function_edits() {
        let first = analyze_source(
            "fn other() -> U64 ( 1 ) fn id(x: U64) -> U64 ( x ) fn d() -> U64 ( id(41) )",
            "d",
        )
        .unwrap();
        let second = analyze_source(
            "fn other() -> U64 ( 2 ) fn id(x: U64) -> U64 ( x ) fn d() -> U64 ( id(41) )",
            "d",
        )
        .unwrap();
        let first_identity = call_code_identity(&first, "id").expect("first id identity");
        let second_identity = call_code_identity(&second, "id").expect("second id identity");
        assert_eq!(
            first_identity.value, second_identity.value,
            "unreferenced function edits must not invalidate this function identity"
        );
    }

    #[test]
    fn direct_function_call_identity_is_shared_by_recursive_scc() {
        let graph = analyze_source(
            "fn f(x: U64) -> U64 ( g(x) ) \
             fn g(x: U64) -> U64 ( f(x) ) \
             fn d() -> U64 ( f(1) )",
            "d",
        )
        .unwrap();
        let f_identity = call_code_identity(&graph, "f").expect("f identity");
        let g_identity = call_code_identity(&graph, "g").expect("g identity");
        assert_eq!(
            f_identity.value, g_identity.value,
            "mutually recursive functions must share one SCC code identity"
        );
    }

    #[test]
    fn named_projection_resolves_through_call_wrapper() {
        let src = "struct Pair { a: U64, b: U64 } \
                   fn pair() -> Pair ( Pair { a: 💣, b: 42 } ) \
                   fn d() -> U64 ( pair().b )";
        bombs_unreached(&analyze_source(src, "d").unwrap());
    }

    #[test]
    fn closure_call_leaves_a_graph_call_node() {
        let g = analyze_source("fn d() -> U64 ( let f = (x) => x + 1; f(41) )", "d").unwrap();
        let code_identity = call_code_identity(&g, "f").expect("f call identity");
        assert_eq!(code_identity.arg_count, 1);
        assert_eq!(code_identity.capture_count, 0);
        assert!(
            code_identity.value.starts_with("closure:blake3:"),
            "closure calls must leave a source-blind Call node for runtime memo hooks: {:?}",
            g.nodes
        );
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
