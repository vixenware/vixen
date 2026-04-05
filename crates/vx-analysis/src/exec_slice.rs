use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use vx_parser::{AstNode, CallExpr, Expr, FieldExpr, FnDecl, RangeExpr, Stmt, SyntaxKind};
use vx_resolve::SourceSpan;

use crate::{
    AnalysisResult, AnalyzedBody, BodyId, BridgeLocalBindingId, BridgeLocalBindingKind,
    ExecSliceError::UnsupportedShape, ExprId, KernelId, RuntimeBoundaryKind, RuntimeNode,
    RuntimeNodeId, SingleSourceBridge, SourceExpr, SourceLocalCalleeOrigin,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableProgram {
    pub entry: BodyId,
    pub bodies: BTreeMap<BodyId, ExecutableBody>,
    pub runtime_nodes: BTreeMap<RuntimeNodeId, ExecutableRuntimeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableBody {
    pub id: BodyId,
    pub kernel: KernelId,
    pub params: Vec<SlotId>,
    pub ops: Vec<KernelOp>,
    pub result_source: ResultSource,
    pub runtime_boundary_extractions: Vec<RuntimeNodeId>,
    pub local_call_dependencies: Vec<BodyId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableClosure {
    pub params: Vec<SlotId>,
    pub ops: Vec<KernelOp>,
    pub result_source: ResultSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableRuntimeNode {
    pub id: RuntimeNodeId,
    pub kind: RuntimeBoundaryKind,
    pub source: SourceExpr,
    pub payload: RuntimeBoundaryPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeBoundaryPayload {
    Exec(ExecBoundaryPayload),
    Discover(DiscoverBoundaryPayload),
    SharedKeyed(SharedKeyedBoundaryPayload),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecBoundaryPayload {
    pub positional_arg_count: usize,
    pub has_where_clause: bool,
    pub has_mounts: bool,
    pub has_writable: bool,
    pub has_coarse_mounts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverBoundaryPayload {
    pub positional_arg_count: usize,
    pub has_closure_arg: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedKeyedBoundaryPayload {
    pub positional_arg_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultSource {
    Slot(SlotId),
    PendingRuntimeBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Slot(SlotId),
    Int(i64),
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeField {
    ExitCode,
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMemberOp {
    Collect,
    Blob,
    Which,
    ExecSubpath,
    ReadSubpath,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeFilePath(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelOp {
    AssignHostLocal {
        dst: SlotId,
        source: SourceExpr,
    },
    AssignSandboxTreeRoot {
        dst: SlotId,
        source: SourceExpr,
        node: RuntimeNodeId,
    },
    AssignConstInt {
        dst: SlotId,
        source: SourceExpr,
        value: i64,
    },
    AssignConstString {
        dst: SlotId,
        source: SourceExpr,
        value: String,
    },
    MakeBlob {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
    },
    AssignAlias {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
    },
    AssignLazyPoison {
        dst: SlotId,
        source: SourceExpr,
        label: String,
    },
    AssignCallableAddConst {
        dst: SlotId,
        source: SourceExpr,
        addend: Operand,
    },
    ApplyCallable {
        dst: SlotId,
        source: SourceExpr,
        callable: Operand,
        arg: Operand,
    },
    MakePair {
        dst: SlotId,
        source: SourceExpr,
        first: Operand,
        second: Operand,
    },
    ProjectIndex {
        dst: SlotId,
        source: SourceExpr,
        pair: Operand,
        index: usize,
    },
    Add {
        dst: SlotId,
        source: SourceExpr,
        lhs: Operand,
        rhs: Operand,
    },
    NotEqual {
        dst: SlotId,
        source: SourceExpr,
        lhs: Operand,
        rhs: Operand,
    },
    ProjectField {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
        field: RuntimeField,
    },
    CallMember {
        dst: SlotId,
        source: SourceExpr,
        receiver: Operand,
        method: RuntimeMemberOp,
        args: Vec<Operand>,
    },
    BuildTree {
        dst: SlotId,
        source: SourceExpr,
        entries: Vec<(TreeFilePath, Operand)>,
    },
    MakeArray {
        dst: SlotId,
        source: SourceExpr,
        items: Vec<Operand>,
    },
    ConcatStrings {
        dst: SlotId,
        source: SourceExpr,
        parts: Vec<Operand>,
    },
    StringLen {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
    },
    StringSlice {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
        start: Operand,
        end: Operand,
    },
    Select {
        dst: SlotId,
        source: SourceExpr,
        condition: Operand,
        then_value: Operand,
        else_value: Operand,
    },
    AssignFailure {
        dst: SlotId,
        source: SourceExpr,
        message: Operand,
    },
    CallBody {
        dst: SlotId,
        source: SourceExpr,
        callee: BodyId,
        args: Vec<Operand>,
    },
    AwaitRuntime {
        dst: SlotId,
        source: SourceExpr,
        node: RuntimeNodeId,
        positional_inputs: Vec<RuntimeBoundaryInputSpec>,
        mounts: Vec<(TreeFilePath, RuntimeBoundaryInputSpec)>,
        writable_roots: Vec<TreeFilePath>,
        coarse_mounts: Vec<RuntimeBoundaryInputSpec>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeBoundaryInputSpec {
    Binding {
        binding: BridgeLocalBindingId,
        slot: SlotId,
    },
    UnresolvedBinding {
        binding: BridgeLocalBindingId,
    },
    IntLiteral(i64),
    StringLiteral(String),
    PathLiteral(TreeFilePath),
    Closure {
        source: SourceExpr,
        closure: ExecutableClosure,
    },
    Array(Vec<RuntimeBoundaryInputSpec>),
    OpaqueExpr {
        source: SourceExpr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecSliceError {
    UnsupportedShape(String),
    MissingBody(BodyId),
    MissingSlot(SlotId),
    MissingExpr(SourceExpr),
    MissingBindingForUse(SourceExpr),
    MissingBindingInitializer(BridgeLocalBindingId),
    TypeMismatch(String),
    VerificationFailure(String),
    InvalidPairIndex(usize),
    InvalidProgram(String),
}

pub fn lower_executable_program(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) -> Result<ExecutableProgram, ExecSliceError> {
    let expr_nodes = expr_nodes_by_id(bridge);
    let fn_by_body = body_nodes_by_id(bridge)?;
    let runtime_nodes = executable_runtime_nodes(analysis, &expr_nodes);
    let mut bodies = BTreeMap::new();
    let lowerable_bodies = analysis
        .bodies
        .iter()
        .map(|body| body.id)
        .collect::<BTreeSet<_>>();
    let body_source_by_id = bridge
        .bodies
        .iter()
        .map(|body| (body.id, body.span.source))
        .collect::<HashMap<_, _>>();

    for analyzed in &analysis.bodies {
        let runtime_boundary_extractions = runtime_pending_for_body(analysis, analyzed.id);
        let kernel = analyzed
            .kernel_entries
            .first()
            .map(|entry| entry.kernel)
            .ok_or_else(|| {
                UnsupportedShape(format!("body {} has no kernel entry", analyzed.id.0))
            })?;

        let fn_decl = fn_by_body.get(&analyzed.id).ok_or_else(|| {
            UnsupportedShape(format!("no AST function for body {}", analyzed.id.0))
        })?;
        let entry_expr = analyzed
            .kernel_entries
            .first()
            .map(|entry| entry.source.expr)
            .ok_or_else(|| {
                UnsupportedShape(format!("body {} has no kernel entry", analyzed.id.0))
            })?;

        let lowered = BodyLowering::new(
            analyzed.id,
            analyzed,
            fn_decl.clone(),
            bridge,
            &expr_nodes,
            entry_expr,
            *body_source_by_id.get(&analyzed.id).ok_or_else(|| {
                UnsupportedShape(format!("missing source id for body {}", analyzed.id.0))
            })?,
            &lowerable_bodies,
        )?
        .lower()?;

        bodies.insert(
            analyzed.id,
            ExecutableBody {
                id: analyzed.id,
                kernel,
                params: lowered.params,
                ops: lowered.ops,
                result_source: ResultSource::Slot(lowered.result),
                runtime_boundary_extractions,
                local_call_dependencies: lowered.local_call_dependencies,
            },
        );
    }

    let entry = pick_entry_body(analysis, bridge)?;
    Ok(ExecutableProgram {
        entry,
        bodies,
        runtime_nodes,
    })
}

pub fn lower_executable_program_for_entry_body(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
    entry: BodyId,
) -> Result<ExecutableProgram, ExecSliceError> {
    let expr_nodes = expr_nodes_by_id(bridge);
    let fn_by_body = body_nodes_by_id(bridge)?;
    let runtime_nodes = executable_runtime_nodes(analysis, &expr_nodes);
    let mut bodies = BTreeMap::new();
    let lowerable_bodies = reachable_body_slice(analysis, entry);
    let body_source_by_id = bridge
        .bodies
        .iter()
        .map(|body| (body.id, body.span.source))
        .collect::<HashMap<_, _>>();

    for analyzed in analysis
        .bodies
        .iter()
        .filter(|body| lowerable_bodies.contains(&body.id))
    {
        let runtime_boundary_extractions = runtime_pending_for_body(analysis, analyzed.id);
        let kernel = analyzed
            .kernel_entries
            .first()
            .map(|entry| entry.kernel)
            .ok_or_else(|| {
                UnsupportedShape(format!("body {} has no kernel entry", analyzed.id.0))
            })?;

        let fn_decl = fn_by_body.get(&analyzed.id).ok_or_else(|| {
            UnsupportedShape(format!("no AST function for body {}", analyzed.id.0))
        })?;
        let entry_expr = analyzed
            .kernel_entries
            .first()
            .map(|entry| entry.source.expr)
            .ok_or_else(|| {
                UnsupportedShape(format!("body {} has no kernel entry", analyzed.id.0))
            })?;

        let lowered = BodyLowering::new(
            analyzed.id,
            analyzed,
            fn_decl.clone(),
            bridge,
            &expr_nodes,
            entry_expr,
            *body_source_by_id.get(&analyzed.id).ok_or_else(|| {
                UnsupportedShape(format!("missing source id for body {}", analyzed.id.0))
            })?,
            &lowerable_bodies,
        )?
        .lower()?;

        bodies.insert(
            analyzed.id,
            ExecutableBody {
                id: analyzed.id,
                kernel,
                params: lowered.params,
                ops: lowered.ops,
                result_source: ResultSource::Slot(lowered.result),
                runtime_boundary_extractions,
                local_call_dependencies: lowered.local_call_dependencies,
            },
        );
    }

    Ok(ExecutableProgram {
        entry,
        bodies,
        runtime_nodes,
    })
}

fn expr_nodes_by_id(bridge: &SingleSourceBridge) -> HashMap<ExprId, Expr> {
    let mut span_to_expr = HashMap::<SourceSpan, ExprId>::new();
    for expr in &bridge.exprs {
        span_to_expr.insert(expr.span, expr.id);
    }

    let mut by_id = HashMap::new();
    for (source_id, file) in &bridge.source_files {
        for expr in file.syntax().descendants().filter_map(Expr::cast) {
            let span =
                SourceSpan::new(vx_resolve::SourceId(*source_id), expr.syntax().text_range());
            if let Some(expr_id) = span_to_expr.get(&span).copied() {
                by_id.insert(expr_id, expr);
            }
        }
    }
    by_id
}

fn body_nodes_by_id(
    bridge: &SingleSourceBridge,
) -> Result<HashMap<BodyId, FnDecl>, ExecSliceError> {
    let mut body_by_span = HashMap::<SourceSpan, BodyId>::new();
    for body in &bridge.bodies {
        body_by_span.insert(body.span, body.id);
    }

    let mut fn_by_body = HashMap::new();
    for (source_id, file) in &bridge.source_files {
        for item in file.items() {
            let vx_parser::Item::FnDecl(fn_decl) = item else {
                continue;
            };
            let span = SourceSpan::new(
                vx_resolve::SourceId(*source_id),
                fn_decl.syntax().text_range(),
            );
            let Some(body_id) = body_by_span.get(&span).copied() else {
                continue;
            };
            fn_by_body.insert(body_id, fn_decl);
        }
    }

    if fn_by_body.is_empty() {
        return Err(UnsupportedShape(
            "bridge has no lowerable function declarations".to_owned(),
        ));
    }
    Ok(fn_by_body)
}

fn reachable_body_slice(analysis: &AnalysisResult, entry: BodyId) -> BTreeSet<BodyId> {
    let mut reachable = BTreeSet::new();
    let mut queue = vec![entry];

    while let Some(body_id) = queue.pop() {
        if !reachable.insert(body_id) {
            continue;
        }
        let Some(body) = analysis.bodies.iter().find(|body| body.id == body_id) else {
            continue;
        };
        for edge in &body.source_local_call_edges {
            let SourceLocalCalleeOrigin::TopLevelBody(callee) = edge.callee else {
                continue;
            };
            if !reachable.contains(&callee) {
                queue.push(callee);
            }
        }
    }

    reachable
}

fn executable_runtime_nodes(
    analysis: &AnalysisResult,
    expr_nodes: &HashMap<ExprId, Expr>,
) -> BTreeMap<RuntimeNodeId, ExecutableRuntimeNode> {
    let mut nodes = BTreeMap::new();
    for node in &analysis.runtime_nodes {
        let Some(kind) = node.boundary_kind() else {
            continue;
        };
        let (id, source) = match node {
            RuntimeNode::Exec { id, source }
            | RuntimeNode::Discover { id, source }
            | RuntimeNode::SharedKeyed { id, source, .. } => (*id, source.clone()),
            RuntimeNode::Lazy { .. } => continue,
        };
        let payload = runtime_boundary_payload(kind, source.expr, expr_nodes);
        nodes.insert(
            id,
            ExecutableRuntimeNode {
                id,
                kind,
                source,
                payload,
            },
        );
    }
    nodes
}

fn runtime_boundary_payload(
    kind: RuntimeBoundaryKind,
    expr_id: ExprId,
    expr_nodes: &HashMap<ExprId, Expr>,
) -> RuntimeBoundaryPayload {
    let call_expr = runtime_call_expr(expr_nodes.get(&expr_id));
    match kind {
        RuntimeBoundaryKind::Exec => RuntimeBoundaryPayload::Exec(ExecBoundaryPayload {
            positional_arg_count: call_expr
                .as_ref()
                .map(|call| call.positional_args().len())
                .unwrap_or(0),
            has_where_clause: matches!(expr_nodes.get(&expr_id), Some(Expr::Where(_))),
            has_mounts: expr_nodes
                .get(&expr_id)
                .and_then(|expr| match expr {
                    Expr::Where(where_expr) => where_expr.keywords(),
                    _ => None,
                })
                .and_then(|keywords| extract_where_field_expr(&keywords, "mounts").ok())
                .is_some(),
            has_writable: expr_nodes
                .get(&expr_id)
                .and_then(|expr| match expr {
                    Expr::Where(where_expr) => where_expr.keywords(),
                    _ => None,
                })
                .and_then(|keywords| extract_where_field_expr(&keywords, "writable").ok())
                .is_some(),
            has_coarse_mounts: expr_nodes
                .get(&expr_id)
                .and_then(|expr| match expr {
                    Expr::Where(where_expr) => where_expr.keywords(),
                    _ => None,
                })
                .and_then(|keywords| extract_where_field_expr(&keywords, "coarse_mounts").ok())
                .is_some(),
        }),
        RuntimeBoundaryKind::Discover => {
            RuntimeBoundaryPayload::Discover(DiscoverBoundaryPayload {
                positional_arg_count: call_expr
                    .as_ref()
                    .map(|call| call.positional_args().len())
                    .unwrap_or(0),
                has_closure_arg: call_expr
                    .as_ref()
                    .map(|call| {
                        call.positional_args()
                            .iter()
                            .any(|arg| matches!(arg, Expr::Closure(_)))
                    })
                    .unwrap_or(false),
            })
        }
        RuntimeBoundaryKind::SharedKeyed => {
            RuntimeBoundaryPayload::SharedKeyed(SharedKeyedBoundaryPayload {
                positional_arg_count: call_expr
                    .as_ref()
                    .map(|call| call.positional_args().len())
                    .unwrap_or(0),
            })
        }
    }
}

fn runtime_call_expr(expr: Option<&Expr>) -> Option<vx_parser::CallExpr> {
    match expr.cloned() {
        Some(Expr::Call(call)) => Some(call),
        Some(Expr::Where(where_expr)) => {
            let callee = where_expr.callee()?;
            vx_parser::CallExpr::cast(callee.syntax().clone())
        }
        _ => None,
    }
}

fn runtime_pending_for_body(analysis: &AnalysisResult, body: BodyId) -> Vec<RuntimeNodeId> {
    let mut pending = analysis
        .bodies
        .iter()
        .find(|candidate| candidate.id == body)
        .map(|candidate| {
            candidate
                .extracted_nodes
                .iter()
                .filter_map(|node| match node.reason {
                    crate::ExtractedNodeReason::RuntimeBoundary { .. } => Some(node.id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    pending.sort_by_key(|id| id.0);
    pending
}

fn pick_entry_body(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) -> Result<BodyId, ExecSliceError> {
    let mut mains = bridge
        .bodies
        .iter()
        .filter(|body| body.name == "main" && body.span.source == bridge.source_id)
        .map(|body| body.id)
        .collect::<Vec<_>>();
    mains.sort_by_key(|id| id.0);
    mains.dedup();
    if let Some(main) = mains.first().copied() {
        return Ok(main);
    }

    let mut roots = analysis
        .bodies
        .iter()
        .filter(|body| {
            body.result_observation == crate::Observation::NeedValue
                && body.incoming_use_observation == crate::Observation::NoObs
        })
        .map(|body| body.id)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        roots = analysis
            .bodies
            .iter()
            .filter(|body| body.result_observation == crate::Observation::NeedValue)
            .map(|body| body.id)
            .collect::<Vec<_>>();
    }
    roots.sort_by_key(|id| id.0);
    roots.first().copied().ok_or_else(|| {
        UnsupportedShape("analysis has no demanded entry body for execution".to_owned())
    })
}

struct LoweredBody {
    params: Vec<SlotId>,
    ops: Vec<KernelOp>,
    result: SlotId,
    local_call_dependencies: Vec<BodyId>,
}

struct LoweredClosure {
    params: Vec<SlotId>,
    ops: Vec<KernelOp>,
    result: ResultSource,
    local_call_dependencies: Vec<BodyId>,
    awaited_runtime_nodes: Vec<RuntimeNodeId>,
}

struct BodyLowering<'a> {
    body: BodyId,
    body_source_id: vx_resolve::SourceId,
    analyzed: &'a AnalyzedBody,
    bridge: &'a SingleSourceBridge,
    expr_nodes: &'a HashMap<ExprId, Expr>,
    entry_expr: ExprId,
    use_binding_by_expr: HashMap<ExprId, BridgeLocalBindingId>,
    use_binding_by_span: HashMap<SourceSpan, BridgeLocalBindingId>,
    binding_by_name_span: HashMap<SourceSpan, BridgeLocalBindingId>,
    binding_initializers: HashMap<BridgeLocalBindingId, ExprId>,
    slot_by_binding: HashMap<BridgeLocalBindingId, SlotId>,
    runtime_node_by_expr: HashMap<ExprId, RuntimeNodeId>,
    runtime_kind_by_expr: HashMap<ExprId, RuntimeBoundaryKind>,
    lowerable_bodies: BTreeSet<BodyId>,
    params: Vec<SlotId>,
    next_slot: u32,
    ops: Vec<KernelOp>,
    materialized_bindings: HashSet<BridgeLocalBindingId>,
    lazy_fields_by_expr: HashMap<ExprId, BTreeSet<u32>>,
    local_call_dependencies: BTreeSet<BodyId>,
    awaited_runtime_nodes: BTreeSet<RuntimeNodeId>,
}

impl<'a> BodyLowering<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        body: BodyId,
        analyzed: &'a AnalyzedBody,
        fn_decl: FnDecl,
        bridge: &'a SingleSourceBridge,
        expr_nodes: &'a HashMap<ExprId, Expr>,
        entry_expr: ExprId,
        body_source_id: vx_resolve::SourceId,
        lowerable_bodies: &BTreeSet<BodyId>,
    ) -> Result<Self, ExecSliceError> {
        let body_bindings = bridge
            .local_bindings
            .iter()
            .filter(|binding| binding.body == body)
            .collect::<Vec<_>>();

        let mut use_binding_by_expr = HashMap::new();
        for usage in &bridge.local_uses {
            if usage.body == body {
                use_binding_by_expr.insert(usage.expr, usage.binding);
            }
        }
        let local_binding_by_def = body_bindings
            .iter()
            .map(|binding| (binding.def_id, binding.id))
            .collect::<HashMap<_, _>>();
        let mut use_binding_by_span = HashMap::new();
        for resolved in &bridge.resolved_refs {
            let Some(target) = resolved.target.as_ref().ok() else {
                continue;
            };
            let Some(binding) = local_binding_by_def.get(&target.id).copied() else {
                continue;
            };
            use_binding_by_span.insert(resolved.span, binding);
        }
        let binding_by_name_span = body_bindings
            .iter()
            .map(|binding| (binding.name_span, binding.id))
            .collect::<HashMap<_, _>>();

        let mut slot_by_binding = HashMap::new();
        let mut next_slot = 0u32;
        for binding in &body_bindings {
            slot_by_binding.insert(binding.id, SlotId(next_slot));
            next_slot += 1;
        }

        let fn_param_spans = fn_decl
            .parameters()
            .filter_map(|param| {
                param
                    .name()
                    .map(|name| SourceSpan::new(body_source_id, name.text_range()))
            })
            .collect::<HashSet<_>>();
        let mut params = Vec::new();
        for binding in body_bindings.iter().filter(|binding| {
            binding.kind == BridgeLocalBindingKind::Parameter
                && fn_param_spans.contains(&binding.name_span)
        }) {
            params.push(*slot_by_binding.get(&binding.id).expect("slot exists"));
        }
        params.sort_by_key(|slot| slot.0);

        let mut binding_initializers = HashMap::new();
        if let Some(block) = fn_decl.body() {
            let let_bindings = body_bindings
                .iter()
                .filter(|binding| binding.kind == BridgeLocalBindingKind::Let)
                .collect::<Vec<_>>();
            let mut assigned = HashSet::<BridgeLocalBindingId>::new();

            for stmt in block.statements() {
                let Stmt::Let(let_stmt) = stmt else {
                    continue;
                };
                let stmt_span = SourceSpan::new(body_source_id, let_stmt.syntax().text_range());
                let Some(binding_id) = let_bindings
                    .iter()
                    .filter(|binding| !assigned.contains(&binding.id))
                    .find(|binding| {
                        binding.name_span.source == stmt_span.source
                            && stmt_span.range.contains_range(binding.name_span.range)
                    })
                    .map(|binding| binding.id)
                else {
                    continue;
                };
                let Some(expr) = let_stmt.expr() else {
                    continue;
                };
                let span = SourceSpan::new(body_source_id, expr.syntax().text_range());
                let Some(expr_id) = bridge
                    .exprs
                    .iter()
                    .find(|candidate| candidate.span == span)
                    .map(|candidate| candidate.id)
                else {
                    continue;
                };
                binding_initializers.insert(binding_id, expr_id);
                assigned.insert(binding_id);
            }
        }

        let mut runtime_node_by_expr = HashMap::new();
        let mut runtime_kind_by_expr = HashMap::new();
        for extracted in &analyzed.extracted_nodes {
            if let crate::ExtractedNodeReason::RuntimeBoundary { kind } = extracted.reason {
                runtime_node_by_expr.insert(extracted.source.expr, extracted.id);
                runtime_kind_by_expr.insert(extracted.source.expr, kind);
            }
        }
        let mut lazy_fields_by_expr = HashMap::<ExprId, BTreeSet<u32>>::new();
        for extracted in &analyzed.extracted_nodes {
            if let crate::ExtractedNodeReason::LazyProductField {
                field: crate::FieldKey::Index(index),
            } = extracted.reason
            {
                lazy_fields_by_expr
                    .entry(extracted.source.expr)
                    .or_default()
                    .insert(index);
            }
        }

        Ok(Self {
            body,
            body_source_id,
            analyzed,
            bridge,
            expr_nodes,
            entry_expr,
            use_binding_by_expr,
            use_binding_by_span,
            binding_by_name_span,
            binding_initializers,
            slot_by_binding,
            runtime_node_by_expr,
            runtime_kind_by_expr,
            lowerable_bodies: lowerable_bodies.clone(),
            params,
            next_slot,
            ops: vec![],
            materialized_bindings: HashSet::new(),
            lazy_fields_by_expr,
            local_call_dependencies: BTreeSet::new(),
            awaited_runtime_nodes: BTreeSet::new(),
        })
    }

    fn lower(mut self) -> Result<LoweredBody, ExecSliceError> {
        let result_operand = self.lower_expr(self.entry_expr)?;
        let result = self.ensure_slot(result_operand, self.source_for(self.entry_expr));
        Ok(LoweredBody {
            params: self.params,
            ops: self.ops,
            result,
            local_call_dependencies: self.local_call_dependencies.into_iter().collect(),
        })
    }

    fn lower_closure(
        mut self,
        closure_param_bindings: Vec<BridgeLocalBindingId>,
    ) -> Result<LoweredClosure, ExecSliceError> {
        self.params = closure_param_bindings
            .iter()
            .map(|binding| {
                self.slot_by_binding.get(binding).copied().ok_or_else(|| {
                    UnsupportedShape(format!("missing slot for binding {}", binding.0))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.materialized_bindings = closure_param_bindings.into_iter().collect();
        let result_operand = self.lower_expr(self.entry_expr)?;
        let result = self.ensure_slot(result_operand, self.source_for(self.entry_expr));
        Ok(LoweredClosure {
            params: self.params,
            ops: self.ops,
            result: ResultSource::Slot(result),
            local_call_dependencies: self.local_call_dependencies.into_iter().collect(),
            awaited_runtime_nodes: self.awaited_runtime_nodes.into_iter().collect(),
        })
    }

    fn source_for(&self, expr_id: ExprId) -> SourceExpr {
        SourceExpr {
            body: self.body,
            expr: expr_id,
        }
    }

    fn expr(&self, expr_id: ExprId) -> Result<Expr, ExecSliceError> {
        self.expr_nodes
            .get(&expr_id)
            .cloned()
            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))
    }

    fn expr_id(&self, expr: &Expr) -> Option<ExprId> {
        let span = SourceSpan::new(self.body_source_id, expr.syntax().text_range());
        self.bridge
            .exprs
            .iter()
            .find(|candidate| candidate.span == span)
            .map(|candidate| candidate.id)
    }

    fn alloc_temp(&mut self) -> SlotId {
        let slot = SlotId(self.next_slot);
        self.next_slot += 1;
        slot
    }

    fn lower_string_member_call(
        &mut self,
        expr_id: ExprId,
        source: &SourceExpr,
        field: FieldExpr,
        call: CallExpr,
        method_name: &str,
    ) -> Result<Option<Operand>, ExecSliceError> {
        let receiver_expr = field
            .receiver()
            .ok_or_else(|| UnsupportedShape("member call receiver missing".to_owned()))?;
        let receiver_id = self
            .expr_id(&receiver_expr)
            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
        let receiver = self.lower_expr(receiver_id)?;
        match method_name {
            "len" => {
                if !self.is_empty_call_arg(call.arg()) {
                    return Err(UnsupportedShape(
                        "String.len does not take positional arguments".to_owned(),
                    ));
                }
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::StringLen {
                    dst,
                    source: source.clone(),
                    value: receiver,
                });
                Ok(Some(Operand::Slot(dst)))
            }
            "slice" => {
                let range = self.extract_string_slice_range(call)?;
                if range.is_inclusive() {
                    return Err(UnsupportedShape(
                        "inclusive string slice ranges are not supported".to_owned(),
                    ));
                }
                let (start, end) = self.lower_slice_range(expr_id, range)?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::StringSlice {
                    dst,
                    source: source.clone(),
                    value: receiver,
                    start,
                    end,
                });
                Ok(Some(Operand::Slot(dst)))
            }
            _ => Ok(None),
        }
    }

    fn extract_string_slice_range(&self, call: CallExpr) -> Result<RangeExpr, ExecSliceError> {
        let arg = call.arg().ok_or_else(|| {
            UnsupportedShape("String.slice expects exactly one range argument".to_owned())
        })?;
        let arg = match arg {
            Expr::Paren(paren) => paren.expr().ok_or_else(|| {
                UnsupportedShape("String.slice range expression missing".to_owned())
            })?,
            other => other,
        };
        match arg {
            Expr::Range(range) => Ok(range),
            Expr::Tuple(tuple) => {
                let mut items = tuple.items();
                let first = items.next().ok_or_else(|| {
                    UnsupportedShape("String.slice expects a range argument".to_owned())
                })?;
                if items.next().is_some() {
                    return Err(UnsupportedShape(
                        "String.slice expects exactly one range argument".to_owned(),
                    ));
                }
                let Expr::Range(range) = first else {
                    return Err(UnsupportedShape(
                        "String.slice expects a range argument".to_owned(),
                    ));
                };
                Ok(range)
            }
            _ => Err(UnsupportedShape(
                "String.slice expects a range argument".to_owned(),
            )),
        }
    }

    fn is_empty_call_arg(&self, arg: Option<Expr>) -> bool {
        match arg {
            None => true,
            Some(Expr::Tuple(tuple)) => tuple.items().next().is_none(),
            _ => false,
        }
    }

    fn lower_slice_range(
        &mut self,
        expr_id: ExprId,
        range: RangeExpr,
    ) -> Result<(Operand, Operand), ExecSliceError> {
        let start_expr = range
            .start()
            .ok_or_else(|| UnsupportedShape("String.slice range start missing".to_owned()))?;
        let end_expr = range
            .end()
            .ok_or_else(|| UnsupportedShape("String.slice range end missing".to_owned()))?;
        let start_id = self
            .expr_id(&start_expr)
            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
        let end_id = self
            .expr_id(&end_expr)
            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
        Ok((self.lower_expr(start_id)?, self.lower_expr(end_id)?))
    }

    fn ensure_slot(&mut self, operand: Operand, source: SourceExpr) -> SlotId {
        match operand {
            Operand::Slot(slot) => slot,
            Operand::Int(value) => {
                let slot = self.alloc_temp();
                self.ops.push(KernelOp::AssignConstInt {
                    dst: slot,
                    source,
                    value,
                });
                slot
            }
            Operand::String(value) => {
                let slot = self.alloc_temp();
                self.ops.push(KernelOp::AssignConstString {
                    dst: slot,
                    source,
                    value,
                });
                slot
            }
        }
    }

    fn materialize_binding(
        &mut self,
        binding: BridgeLocalBindingId,
    ) -> Result<SlotId, ExecSliceError> {
        let slot = *self
            .slot_by_binding
            .get(&binding)
            .ok_or_else(|| UnsupportedShape(format!("missing slot for binding {}", binding.0)))?;

        if self.materialized_bindings.contains(&binding) {
            return Ok(slot);
        }

        let Some(init_expr) = self.binding_initializers.get(&binding).copied() else {
            self.materialized_bindings.insert(binding);
            return Ok(slot);
        };

        let source = self.source_for(init_expr);
        let value = self.lower_expr(init_expr)?;
        if !(matches!(value, Operand::Slot(existing) if existing == slot)) {
            self.ops.push(KernelOp::AssignAlias {
                dst: slot,
                source,
                value,
            });
        }
        self.materialized_bindings.insert(binding);
        Ok(slot)
    }

    fn closure_param_bindings(
        &self,
        closure: &vx_parser::ClosureExpr,
    ) -> Result<Vec<BridgeLocalBindingId>, ExecSliceError> {
        let mut bindings = Vec::new();
        for param in closure.parameters() {
            let name = param
                .name()
                .ok_or_else(|| UnsupportedShape("closure parameter missing".to_owned()))?;
            let span = SourceSpan::new(self.body_source_id, name.text_range());
            let binding = self
                .binding_by_name_span
                .get(&span)
                .copied()
                .ok_or_else(|| {
                    UnsupportedShape(format!(
                        "missing typed binding identity for closure parameter `{}`",
                        name.text()
                    ))
                })?;
            bindings.push(binding);
        }
        Ok(bindings)
    }

    fn lower_closure_with_bound_operands(
        &mut self,
        closure: &vx_parser::ClosureExpr,
        bound_operands: Vec<Operand>,
        source_expr: ExprId,
    ) -> Result<Operand, ExecSliceError> {
        let bindings = self.closure_param_bindings(closure)?;
        if bindings.len() != bound_operands.len() {
            return Err(UnsupportedShape(format!(
                "closure expects {} params, got {} bound values",
                bindings.len(),
                bound_operands.len()
            )));
        }
        let body = closure
            .body()
            .ok_or_else(|| UnsupportedShape("closure body missing".to_owned()))?;
        let body_id = self
            .expr_id(&body)
            .ok_or(ExecSliceError::MissingExpr(self.source_for(source_expr)))?;
        for (binding, value) in bindings.into_iter().zip(bound_operands) {
            let slot = self.materialize_binding(binding)?;
            self.ops.push(KernelOp::AssignAlias {
                dst: slot,
                source: self.source_for(source_expr),
                value,
            });
        }
        self.lower_expr(body_id)
    }

    fn shipped_closure_for_expr(
        &self,
        closure: &vx_parser::ClosureExpr,
        source_expr: ExprId,
    ) -> Result<ExecutableClosure, ExecSliceError> {
        let param_bindings = self.closure_param_bindings(closure)?;
        let body = closure
            .body()
            .ok_or_else(|| UnsupportedShape("closure body missing".to_owned()))?;
        let body_id = self
            .expr_id(&body)
            .ok_or(ExecSliceError::MissingExpr(self.source_for(source_expr)))?;

        let lowered = BodyLowering {
            bridge: self.bridge,
            analyzed: self.analyzed,
            body: self.body,
            body_source_id: self.body_source_id,
            expr_nodes: self.expr_nodes,
            entry_expr: body_id,
            use_binding_by_expr: self.use_binding_by_expr.clone(),
            use_binding_by_span: self.use_binding_by_span.clone(),
            binding_by_name_span: self.binding_by_name_span.clone(),
            binding_initializers: self.binding_initializers.clone(),
            slot_by_binding: self.slot_by_binding.clone(),
            runtime_node_by_expr: self.runtime_node_by_expr.clone(),
            runtime_kind_by_expr: self.runtime_kind_by_expr.clone(),
            lowerable_bodies: self.lowerable_bodies.clone(),
            params: vec![],
            next_slot: self.next_slot,
            ops: vec![],
            materialized_bindings: HashSet::new(),
            lazy_fields_by_expr: self.lazy_fields_by_expr.clone(),
            local_call_dependencies: BTreeSet::new(),
            awaited_runtime_nodes: BTreeSet::new(),
        }
        .lower_closure(param_bindings)?;

        if !lowered.local_call_dependencies.is_empty() {
            return Err(UnsupportedShape(
                "shipped closures with local body-call dependencies are unsupported".to_owned(),
            ));
        }
        if !lowered.awaited_runtime_nodes.is_empty() {
            return Err(UnsupportedShape(
                "shipped closures with nested runtime waits are unsupported".to_owned(),
            ));
        }

        Ok(ExecutableClosure {
            params: lowered.params,
            ops: lowered.ops,
            result_source: lowered.result,
        })
    }

    fn lower_expr(&mut self, expr_id: ExprId) -> Result<Operand, ExecSliceError> {
        let expr = self.expr(expr_id)?;
        match expr {
            Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or_else(|| UnsupportedShape("empty paren expression".to_owned()))?;
                let inner_id = self
                    .expr_id(&inner)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                self.lower_expr(inner_id)
            }
            Expr::Where(where_expr) => {
                if let Some(keywords) = where_expr.keywords() {
                    let observer = extract_observer_closure(&keywords)?;
                    let callee = where_expr.callee().ok_or_else(|| {
                        UnsupportedShape("where expression missing callee".to_owned())
                    })?;
                    let callee_id = self
                        .expr_id(&callee)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                    let runtime_node = self.runtime_node_by_expr.get(&callee_id).copied();
                    let runtime_kind = self.runtime_kind_by_expr.get(&callee_id).copied();
                    let callee_value = if runtime_kind == Some(RuntimeBoundaryKind::Exec) {
                        let call = vx_parser::CallExpr::cast(callee.syntax().clone()).ok_or_else(
                            || {
                                UnsupportedShape(
                                    "runtime boundary where-expression must wrap a call".to_owned(),
                                )
                            },
                        )?;
                        let positional_inputs = call
                            .positional_args()
                            .into_iter()
                            .map(|arg| self.runtime_input_spec_for_expr(&arg))
                            .collect::<Result<Vec<_>, _>>()?;
                        let mounts = match extract_where_field_expr(&keywords, "mounts") {
                            Ok(expr) => self.runtime_mount_specs_for_expr(&expr)?,
                            Err(ExecSliceError::UnsupportedShape(_)) => vec![],
                            Err(err) => return Err(err),
                        };
                        let writable_roots = match extract_where_field_expr(&keywords, "writable") {
                            Ok(expr) => self.runtime_path_list_for_expr(&expr)?,
                            Err(ExecSliceError::UnsupportedShape(_)) => vec![],
                            Err(err) => return Err(err),
                        };
                        let coarse_mounts =
                            match extract_where_field_expr(&keywords, "coarse_mounts") {
                                Ok(expr) => match self.runtime_input_spec_for_expr(&expr)? {
                                    RuntimeBoundaryInputSpec::Array(items) => items,
                                    other => vec![other],
                                },
                                Err(ExecSliceError::UnsupportedShape(_)) => vec![],
                                Err(err) => return Err(err),
                            };
                        let node = runtime_node.ok_or_else(|| {
                            UnsupportedShape(
                                "exec observer requires concrete exec runtime node".to_owned(),
                            )
                        })?;
                        let dst = self.alloc_temp();
                        self.ops.push(KernelOp::AwaitRuntime {
                            dst,
                            source: self.source_for(callee_id),
                            node,
                            positional_inputs,
                            mounts,
                            writable_roots,
                            coarse_mounts,
                        });
                        self.awaited_runtime_nodes.insert(node);
                        Operand::Slot(dst)
                    } else {
                        self.lower_expr(callee_id)?
                    };
                    let param_bindings = self.closure_param_bindings(&observer)?;
                    if param_bindings.len() > 2 {
                        return Err(UnsupportedShape(format!(
                            "observer closure supports up to 2 parameters, got {}",
                            param_bindings.len()
                        )));
                    }
                    let mut bound_operands = Vec::with_capacity(param_bindings.len());
                    if !param_bindings.is_empty() {
                        bound_operands.push(callee_value.clone());
                    }
                    if param_bindings.len() >= 2 {
                        if runtime_kind != Some(RuntimeBoundaryKind::Exec) {
                            return Err(UnsupportedShape(
                                "observer second parameter requires exec runtime boundary context"
                                    .to_owned(),
                            ));
                        }
                        let node = runtime_node.ok_or_else(|| {
                            UnsupportedShape(
                                "observer second parameter requires concrete exec runtime node"
                                    .to_owned(),
                            )
                        })?;
                        let sandbox_root = self.alloc_temp();
                        self.ops.push(KernelOp::AssignSandboxTreeRoot {
                            dst: sandbox_root,
                            source: self.source_for(expr_id),
                            node,
                        });
                        bound_operands.push(Operand::Slot(sandbox_root));
                    }
                    return self.lower_closure_with_bound_operands(
                        &observer,
                        bound_operands,
                        expr_id,
                    );
                }
                if let Some(node) = self.runtime_node_by_expr.get(&expr_id).copied() {
                    let call = where_expr
                        .callee()
                        .and_then(|callee| vx_parser::CallExpr::cast(callee.syntax().clone()))
                        .ok_or_else(|| {
                            UnsupportedShape(
                                "runtime boundary where-expression must wrap a call".to_owned(),
                            )
                        })?;
                    let positional_inputs = call
                        .positional_args()
                        .into_iter()
                        .map(|arg| self.runtime_input_spec_for_expr(&arg))
                        .collect::<Result<Vec<_>, _>>()?;
                    let dst = self.alloc_temp();
                    self.ops.push(KernelOp::AwaitRuntime {
                        dst,
                        source: self.source_for(expr_id),
                        node,
                        positional_inputs,
                        mounts: vec![],
                        writable_roots: vec![],
                        coarse_mounts: vec![],
                    });
                    self.awaited_runtime_nodes.insert(node);
                    return Ok(Operand::Slot(dst));
                }
                let callee = where_expr.callee().ok_or_else(|| {
                    UnsupportedShape("where expression missing callee".to_owned())
                })?;
                let callee_id = self
                    .expr_id(&callee)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                self.lower_expr(callee_id)
            }
            Expr::Block(block) => {
                let statements = block.statements().collect::<Vec<_>>();
                let last_expr = statements.iter().rev().find_map(|stmt| match stmt {
                    Stmt::Expr(expr) => Some(expr.clone()),
                    Stmt::Return(ret) => ret.expr(),
                    _ => None,
                });
                let Some(last_expr) = last_expr else {
                    return Err(UnsupportedShape(
                        "block has no result expression".to_owned(),
                    ));
                };
                let last_id = self
                    .expr_id(&last_expr)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                self.lower_expr(last_id)
            }
            Expr::Literal(literal) => {
                let token = literal
                    .token()
                    .ok_or_else(|| UnsupportedShape("literal without token".to_owned()))?;
                match token.kind() {
                    SyntaxKind::INT => {
                        let value = token.text().replace('_', "").parse::<i64>().map_err(|_| {
                            UnsupportedShape(format!("invalid int `{}`", token.text()))
                        })?;
                        Ok(Operand::Int(value))
                    }
                    SyntaxKind::STRING => Ok(Operand::String(normalize_string_literal_token(
                        token.text(),
                    ))),
                    _ => Err(UnsupportedShape(format!(
                        "only INT/STRING literals supported, got `{}`",
                        token.text()
                    ))),
                }
            }
            Expr::NameRef(_) => {
                let binding = self
                    .use_binding_by_expr
                    .get(&expr_id)
                    .copied()
                    .or_else(|| {
                        let expr = self.expr_nodes.get(&expr_id)?;
                        let span = SourceSpan::new(self.body_source_id, expr.syntax().text_range());
                        self.use_binding_by_span.get(&span).copied()
                    })
                    .ok_or(ExecSliceError::MissingBindingForUse(
                        self.source_for(expr_id),
                    ))?;
                let slot = self.materialize_binding(binding)?;
                Ok(Operand::Slot(slot))
            }
            Expr::Binary(binary) => {
                let source = self.source_for(expr_id);
                let has_plus = binary
                    .syntax()
                    .children_with_tokens()
                    .filter_map(|child| child.into_token())
                    .any(|token| token.kind() == SyntaxKind::PLUS);
                let has_not_equal = binary
                    .syntax()
                    .children_with_tokens()
                    .filter_map(|child| child.into_token())
                    .any(|token| token.kind() == SyntaxKind::BANG_EQ);
                if !has_plus && !has_not_equal {
                    return Err(UnsupportedShape(format!(
                        "only + and != binary are supported in exec slice (expr {})",
                        expr_id.0
                    )));
                }

                let lhs = binary
                    .lhs()
                    .ok_or_else(|| UnsupportedShape("binary lhs missing".to_owned()))?;
                let rhs = binary
                    .rhs()
                    .ok_or_else(|| UnsupportedShape("binary rhs missing".to_owned()))?;
                let lhs = self.lower_expr(
                    self.expr_id(&lhs)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?,
                )?;
                let rhs = self.lower_expr(
                    self.expr_id(&rhs)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?,
                )?;
                let dst = self.alloc_temp();
                if has_plus {
                    self.ops.push(KernelOp::Add {
                        dst,
                        source,
                        lhs,
                        rhs,
                    });
                } else {
                    self.ops.push(KernelOp::NotEqual {
                        dst,
                        source,
                        lhs,
                        rhs,
                    });
                }
                Ok(Operand::Slot(dst))
            }
            Expr::Tuple(tuple) => {
                let items = tuple.items().collect::<Vec<_>>();
                if items.len() != 2 {
                    return Err(UnsupportedShape(format!(
                        "only 2-item tuples supported, got {}",
                        items.len()
                    )));
                }

                let mut operands = Vec::with_capacity(2);
                for (index, item) in items.iter().enumerate() {
                    let item_id = self
                        .expr_id(item)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                    match self.lower_expr(item_id) {
                        Ok(value) => operands.push(value),
                        Err(UnsupportedShape(_))
                            if self
                                .lazy_fields_by_expr
                                .get(&expr_id)
                                .is_some_and(|fields| fields.contains(&(index as u32))) =>
                        {
                            let lazy = self.alloc_temp();
                            self.ops.push(KernelOp::AssignLazyPoison {
                                dst: lazy,
                                source: self.source_for(item_id),
                                label: format!("lazy-field-{}:{}", expr_id.0, index),
                            });
                            operands.push(Operand::Slot(lazy));
                        }
                        Err(err) => return Err(err),
                    }
                }

                let dst = self.alloc_temp();
                self.ops.push(KernelOp::MakePair {
                    dst,
                    source: self.source_for(expr_id),
                    first: operands[0].clone(),
                    second: operands[1].clone(),
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Array(array) => {
                let items = array
                    .items()
                    .map(|item| match item {
                        vx_parser::ArrayItem::Expr(expr) => {
                            let id = self
                                .expr_id(&expr)
                                .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                            self.lower_expr(id)
                        }
                        vx_parser::ArrayItem::Spread(_) => Err(UnsupportedShape(
                            "array spread is outside restricted executable slice".to_owned(),
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::MakeArray {
                    dst,
                    source: self.source_for(expr_id),
                    items,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::InterpolatedString(interp) => {
                let mut parts = Vec::new();
                for part in interp.parts() {
                    match part {
                        vx_parser::StringPart::Text(token) => {
                            if !token.text().is_empty() {
                                parts.push(Operand::String(token.text().to_owned()));
                            }
                        }
                        vx_parser::StringPart::Interpolation(interp) => {
                            let expr = interp.expr().ok_or_else(|| {
                                UnsupportedShape(
                                    "string interpolation missing expression".to_owned(),
                                )
                            })?;
                            let part_id = self
                                .expr_id(&expr)
                                .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                            parts.push(self.lower_expr(part_id)?);
                        }
                    }
                }
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::ConcatStrings {
                    dst,
                    source: self.source_for(expr_id),
                    parts,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Field(field) => {
                let field_name = field
                    .field_name()
                    .ok_or_else(|| UnsupportedShape("field name missing".to_owned()))?
                    .text()
                    .to_owned();
                let receiver = field
                    .receiver()
                    .ok_or_else(|| UnsupportedShape("field receiver missing".to_owned()))?;

                if let Some(index) = field_name.strip_prefix('_') {
                    let index = index
                        .parse::<usize>()
                        .map_err(|_| UnsupportedShape(format!("invalid field `{field_name}`")))?;

                    if let Expr::NameRef(_) = receiver
                        && let Some(receiver_id) = self.expr_id(&receiver)
                        && let Some(binding) = self.use_binding_by_expr.get(&receiver_id).copied()
                        && let Some(tuple_expr) = self.binding_initializers.get(&binding).copied()
                        && let Expr::Tuple(tuple) = self.expr(tuple_expr)?
                    {
                        let items = tuple.items().collect::<Vec<_>>();
                        if index < items.len() {
                            let selected_id = self
                                .expr_id(&items[index])
                                .ok_or(ExecSliceError::MissingExpr(self.source_for(tuple_expr)))?;
                            return self.lower_expr(selected_id);
                        }
                    }

                    let receiver_id = self
                        .expr_id(&receiver)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                    let pair = self.lower_expr(receiver_id)?;
                    let dst = self.alloc_temp();
                    self.ops.push(KernelOp::ProjectIndex {
                        dst,
                        source: self.source_for(expr_id),
                        pair,
                        index,
                    });
                    return Ok(Operand::Slot(dst));
                }

                let receiver_id = self
                    .expr_id(&receiver)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                let value = self.lower_expr(receiver_id)?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::ProjectField {
                    dst,
                    source: self.source_for(expr_id),
                    value,
                    field: runtime_field_from_name(&field_name)?,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::If(if_expr) => {
                let condition = if_expr
                    .condition()
                    .ok_or_else(|| UnsupportedShape("if condition missing".to_owned()))?;
                let then_branch = if_expr
                    .then_branch()
                    .ok_or_else(|| UnsupportedShape("if then branch missing".to_owned()))?;
                let else_branch = if_expr
                    .else_branch()
                    .ok_or_else(|| UnsupportedShape("if else branch missing".to_owned()))?;
                let condition = self.lower_expr(
                    self.expr_id(&condition)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?,
                )?;
                let then_value = self.lower_expr(
                    self.expr_id(&then_branch)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?,
                )?;
                let else_value = self.lower_expr(
                    self.expr_id(&else_branch)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?,
                )?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::Select {
                    dst,
                    source: self.source_for(expr_id),
                    condition,
                    then_value,
                    else_value,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Fail(fail_expr) => {
                let message = if let Some(message) = fail_expr.message() {
                    let message_id = self
                        .expr_id(&message)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                    self.lower_expr(message_id)?
                } else {
                    Operand::String("fail".to_owned())
                };
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::AssignFailure {
                    dst,
                    source: self.source_for(expr_id),
                    message,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Closure(closure) => {
                let param_bindings = self.closure_param_bindings(&closure)?;
                if param_bindings.len() != 1 {
                    return Err(UnsupportedShape(
                        "only single-parameter closure supported".to_owned(),
                    ));
                };
                let param_binding = param_bindings[0];
                let body = closure
                    .body()
                    .ok_or_else(|| UnsupportedShape("closure body missing".to_owned()))?;
                let body_id = self
                    .expr_id(&body)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                let Expr::Binary(binary) = self.expr(body_id)? else {
                    return Err(UnsupportedShape(
                        "only closure body `x + captured` supported".to_owned(),
                    ));
                };
                let lhs = binary
                    .lhs()
                    .ok_or_else(|| UnsupportedShape("closure lhs missing".to_owned()))?;
                let rhs = binary
                    .rhs()
                    .ok_or_else(|| UnsupportedShape("closure rhs missing".to_owned()))?;
                let lhs_id = self
                    .expr_id(&lhs)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(body_id)))?;
                let rhs_id = self
                    .expr_id(&rhs)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(body_id)))?;
                let lhs_is_param =
                    self.use_binding_by_expr.get(&lhs_id).copied() == Some(param_binding);
                let rhs_is_param =
                    self.use_binding_by_expr.get(&rhs_id).copied() == Some(param_binding);
                let addend = if lhs_is_param {
                    self.lower_expr(rhs_id)?
                } else if rhs_is_param {
                    self.lower_expr(lhs_id)?
                } else {
                    return Err(UnsupportedShape(
                        "closure add must include parameter on one side".to_owned(),
                    ));
                };

                let dst = self.alloc_temp();
                self.ops.push(KernelOp::AssignCallableAddConst {
                    dst,
                    source: self.source_for(expr_id),
                    addend,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Call(call) => {
                if let Some(node) = self.runtime_node_by_expr.get(&expr_id).copied() {
                    let positional_inputs = call
                        .positional_args()
                        .into_iter()
                        .map(|arg| self.runtime_input_spec_for_expr(&arg))
                        .collect::<Result<Vec<_>, _>>()?;
                    let dst = self.alloc_temp();
                    self.ops.push(KernelOp::AwaitRuntime {
                        dst,
                        source: self.source_for(expr_id),
                        node,
                        positional_inputs,
                        mounts: vec![],
                        writable_roots: vec![],
                        coarse_mounts: vec![],
                    });
                    self.awaited_runtime_nodes.insert(node);
                    return Ok(Operand::Slot(dst));
                }

                let source = self.source_for(expr_id);
                let callee_expr = call
                    .callee()
                    .ok_or_else(|| UnsupportedShape("call has no callee".to_owned()))?;

                if let Expr::NameRef(name_ref) = callee_expr.clone()
                    && name_ref.token().is_some_and(|token| token.text() == "Blob")
                {
                    if call.positional_args().len() != 1 {
                        return Err(UnsupportedShape(
                            "Blob constructor expects one argument".to_owned(),
                        ));
                    }
                    let value_expr =
                        call.positional_args().into_iter().next().ok_or_else(|| {
                            UnsupportedShape("Blob constructor missing value".to_owned())
                        })?;
                    let value_id = self
                        .expr_id(&value_expr)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                    let value = self.lower_expr(value_id)?;
                    let dst = self.alloc_temp();
                    self.ops.push(KernelOp::MakeBlob { dst, source, value });
                    return Ok(Operand::Slot(dst));
                }

                if let Expr::NameRef(name_ref) = callee_expr.clone()
                    && name_ref.token().is_some_and(|token| token.text() == "Tree")
                {
                    if call.positional_args().len() != 1 {
                        return Err(UnsupportedShape(
                            "Tree constructor expects one map argument".to_owned(),
                        ));
                    }
                    let map_expr = call.positional_args().into_iter().next().ok_or_else(|| {
                        UnsupportedShape("Tree constructor missing map".to_owned())
                    })?;
                    let Expr::Map(map) = map_expr else {
                        return Err(UnsupportedShape(
                            "Tree constructor requires map literal argument".to_owned(),
                        ));
                    };
                    let entries = self.lower_tree_entries(map)?;
                    let dst = self.alloc_temp();
                    self.ops.push(KernelOp::BuildTree {
                        dst,
                        source,
                        entries,
                    });
                    return Ok(Operand::Slot(dst));
                }

                if let Expr::Field(field) = callee_expr.clone()
                    && let Some(method_name) =
                        field.field_name().map(|token| token.text().to_owned())
                {
                    if let Some(lowered) = self.lower_string_member_call(
                        expr_id,
                        &source,
                        field.clone(),
                        call.clone(),
                        &method_name,
                    )? {
                        return Ok(lowered);
                    }

                    if let Some(method) = runtime_member_op_from_name(&method_name) {
                        let receiver_expr = field.receiver().ok_or_else(|| {
                            UnsupportedShape("member call receiver missing".to_owned())
                        })?;
                        let receiver_id = self
                            .expr_id(&receiver_expr)
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                        let receiver = self.lower_expr(receiver_id)?;
                        let args = call
                            .positional_args()
                            .into_iter()
                            .map(|arg| {
                                let id = self
                                    .expr_id(&arg)
                                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                                self.lower_expr(id)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let dst = self.alloc_temp();
                        self.ops.push(KernelOp::CallMember {
                            dst,
                            source,
                            receiver,
                            method,
                            args,
                        });
                        return Ok(Operand::Slot(dst));
                    }
                }

                let args = call
                    .positional_args()
                    .into_iter()
                    .map(|arg| {
                        let id = self
                            .expr_id(&arg)
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                        self.lower_expr(id)
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                if let Some(origin) = self.analyzed.callee_local_origins.get(&expr_id) {
                    match origin {
                        SourceLocalCalleeOrigin::TopLevelBody(callee) => {
                            if !self.lowerable_bodies.contains(callee) {
                                return Err(UnsupportedShape(format!(
                                    "top-level callee body {} outside lowered body set",
                                    callee.0
                                )));
                            }
                            self.local_call_dependencies.insert(*callee);
                            let dst = self.alloc_temp();
                            self.ops.push(KernelOp::CallBody {
                                dst,
                                source,
                                callee: *callee,
                                args,
                            });
                            return Ok(Operand::Slot(dst));
                        }
                        SourceLocalCalleeOrigin::LocalClosure(_)
                        | SourceLocalCalleeOrigin::LocalDef(_) => {}
                    }
                }

                if args.len() != 1 {
                    return Err(UnsupportedShape(format!(
                        "callable apply supports one arg, got {}",
                        args.len()
                    )));
                }
                let callee_id = self
                    .expr_id(&callee_expr)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                let callable = self.lower_expr(callee_id)?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::ApplyCallable {
                    dst,
                    source,
                    callable,
                    arg: args[0].clone(),
                });
                Ok(Operand::Slot(dst))
            }
            other => Err(UnsupportedShape(format!(
                "expr kind {:?} is outside restricted executable slice",
                other
            ))),
        }
    }

    fn runtime_input_spec_for_expr(
        &mut self,
        expr: &Expr,
    ) -> Result<RuntimeBoundaryInputSpec, ExecSliceError> {
        if let Expr::NameRef(_) = expr
            && let Some(expr_id) = self.expr_id(expr)
            && let Some(binding) = self.use_binding_by_expr.get(&expr_id).copied()
        {
            return match self.materialize_binding(binding) {
                Ok(slot) => Ok(RuntimeBoundaryInputSpec::Binding { binding, slot }),
                Err(UnsupportedShape(_)) | Err(ExecSliceError::MissingBindingForUse(_)) => {
                    Ok(RuntimeBoundaryInputSpec::UnresolvedBinding { binding })
                }
                Err(err) => Err(err),
            };
        }

        if let Expr::Literal(literal) = expr
            && let Some(token) = literal.token()
        {
            return match token.kind() {
                SyntaxKind::INT => {
                    let value =
                        token.text().replace('_', "").parse::<i64>().map_err(|_| {
                            UnsupportedShape(format!("invalid int `{}`", token.text()))
                        })?;
                    Ok(RuntimeBoundaryInputSpec::IntLiteral(value))
                }
                SyntaxKind::STRING => Ok(RuntimeBoundaryInputSpec::StringLiteral(
                    normalize_string_literal_token(token.text()),
                )),
                _ => {
                    let expr_id = self.expr_id(expr).ok_or(ExecSliceError::MissingExpr(
                        self.source_for(self.entry_expr),
                    ))?;
                    Ok(RuntimeBoundaryInputSpec::OpaqueExpr {
                        source: self.source_for(expr_id),
                    })
                }
            };
        }

        if let Some(path) = parse_tree_path_key(expr) {
            return Ok(RuntimeBoundaryInputSpec::PathLiteral(path));
        }

        if let Expr::Closure(_) = expr {
            let expr_id = self.expr_id(expr).ok_or(ExecSliceError::MissingExpr(
                self.source_for(self.entry_expr),
            ))?;
            let Expr::Closure(closure) = expr else {
                unreachable!("checked closure expr");
            };
            return Ok(RuntimeBoundaryInputSpec::Closure {
                source: self.source_for(expr_id),
                closure: self.shipped_closure_for_expr(closure, expr_id)?,
            });
        }

        if let Expr::Array(array) = expr {
            let items = array
                .items()
                .map(|item| match item {
                    vx_parser::ArrayItem::Expr(expr) => self.runtime_input_spec_for_expr(&expr),
                    vx_parser::ArrayItem::Spread(spread) => {
                        let source = spread
                            .expr()
                            .and_then(|expr| self.expr_id(&expr))
                            .map(|expr_id| self.source_for(expr_id))
                            .ok_or(ExecSliceError::MissingExpr(
                                self.source_for(self.entry_expr),
                            ))?;
                        Ok(RuntimeBoundaryInputSpec::OpaqueExpr { source })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(RuntimeBoundaryInputSpec::Array(items));
        }

        let expr_id = self.expr_id(expr).ok_or(ExecSliceError::MissingExpr(
            self.source_for(self.entry_expr),
        ))?;
        Ok(RuntimeBoundaryInputSpec::OpaqueExpr {
            source: self.source_for(expr_id),
        })
    }

    fn runtime_mount_specs_for_expr(
        &mut self,
        expr: &Expr,
    ) -> Result<Vec<(TreeFilePath, RuntimeBoundaryInputSpec)>, ExecSliceError> {
        let Expr::Map(map) = expr else {
            return Err(UnsupportedShape(
                "exec mounts must be a map literal in executable slice".to_owned(),
            ));
        };
        let mut mounts = Vec::new();
        for entry in map.entries() {
            if entry.spread().is_some() {
                return Err(UnsupportedShape(
                    "exec mounts spread entries are unsupported in executable slice".to_owned(),
                ));
            }
            let key = entry
                .key()
                .ok_or_else(|| UnsupportedShape("exec mount key missing".to_owned()))?;
            let path = parse_tree_path_key(&key).ok_or_else(|| {
                UnsupportedShape("exec mount key must be a simple path literal".to_owned())
            })?;
            let value = entry
                .value()
                .ok_or_else(|| UnsupportedShape("exec mount value missing".to_owned()))?;
            mounts.push((path, self.runtime_input_spec_for_expr(&value)?));
        }
        Ok(mounts)
    }

    fn runtime_path_list_for_expr(
        &mut self,
        expr: &Expr,
    ) -> Result<Vec<TreeFilePath>, ExecSliceError> {
        let Expr::Array(array) = expr else {
            return Err(UnsupportedShape(
                "exec path list must be an array literal in executable slice".to_owned(),
            ));
        };
        let mut paths = Vec::new();
        for item in array.items() {
            let vx_parser::ArrayItem::Expr(expr) = item else {
                return Err(UnsupportedShape(
                    "exec path lists do not support spread items".to_owned(),
                ));
            };
            let path = parse_tree_path_key(&expr).ok_or_else(|| {
                UnsupportedShape("exec path list items must be path literals".to_owned())
            })?;
            paths.push(path);
        }
        Ok(paths)
    }

    fn lower_tree_entries(
        &mut self,
        map: vx_parser::MapExpr,
    ) -> Result<Vec<(TreeFilePath, Operand)>, ExecSliceError> {
        let mut entries = Vec::new();
        for entry in map.entries() {
            if entry.spread().is_some() {
                return Err(UnsupportedShape(
                    "Tree map spread entries are unsupported in executable slice".to_owned(),
                ));
            }
            let key = entry
                .key()
                .ok_or_else(|| UnsupportedShape("Tree map entry key missing".to_owned()))?;
            let path = parse_tree_path_key(&key).ok_or_else(|| {
                UnsupportedShape("Tree map key must be a simple path literal".to_owned())
            })?;
            let value = entry
                .value()
                .ok_or_else(|| UnsupportedShape("Tree map entry value missing".to_owned()))?;
            let value_id = self.expr_id(&value).ok_or(ExecSliceError::MissingExpr(
                self.source_for(self.entry_expr),
            ))?;
            let lowered = self.lower_expr(value_id)?;
            entries.push((path, lowered));
        }
        Ok(entries)
    }
}

fn parse_tree_path_key(expr: &Expr) -> Option<TreeFilePath> {
    let Expr::PathLiteral(path) = expr else {
        return None;
    };
    let text = path.syntax().text().to_string();
    Some(TreeFilePath(text.trim().trim_start_matches('@').to_owned()))
}

fn runtime_field_from_name(name: &str) -> Result<RuntimeField, ExecSliceError> {
    match name {
        "exit_code" => Ok(RuntimeField::ExitCode),
        "stdout" => Ok(RuntimeField::Stdout),
        "stderr" => Ok(RuntimeField::Stderr),
        _ => Err(UnsupportedShape(format!(
            "unsupported runtime field `{name}` in executable slice"
        ))),
    }
}

fn runtime_member_op_from_name(name: &str) -> Option<RuntimeMemberOp> {
    match name {
        "collect" => Some(RuntimeMemberOp::Collect),
        "blob" => Some(RuntimeMemberOp::Blob),
        "which" => Some(RuntimeMemberOp::Which),
        "exec_subpath" => Some(RuntimeMemberOp::ExecSubpath),
        "read_subpath" => Some(RuntimeMemberOp::ReadSubpath),
        _ => None,
    }
}

fn extract_where_field_expr(keywords: &Expr, field_name: &str) -> Result<Expr, ExecSliceError> {
    let Expr::Record(record) = keywords else {
        return Err(UnsupportedShape(
            "where keywords must be a record for observer execution".to_owned(),
        ));
    };
    record
        .fields()
        .find_map(|field| {
            let name = field.name()?;
            (name.text() == field_name).then(|| field.value()).flatten()
        })
        .ok_or_else(|| UnsupportedShape(format!("where {field_name} keyword missing")))
}

fn extract_observer_closure(keywords: &Expr) -> Result<vx_parser::ClosureExpr, ExecSliceError> {
    let observer_expr = extract_where_field_expr(keywords, "observer")?;
    let Expr::Closure(closure) = observer_expr else {
        return Err(UnsupportedShape(
            "where observer must be a closure expression".to_owned(),
        ));
    };
    Ok(closure)
}

fn normalize_string_literal_token(token_text: &str) -> String {
    let inner = token_text
        .strip_prefix('"')
        .and_then(|stripped| stripped.strip_suffix('"'))
        .unwrap_or(token_text);

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('u') => {
                let Some('{') = chars.next() else {
                    out.push('\\');
                    out.push('u');
                    continue;
                };
                let mut hex = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => hex.push(c),
                        None => {
                            out.push_str("\\u{");
                            out.push_str(&hex);
                            return out;
                        }
                    }
                }
                if let Ok(codepoint) = u32::from_str_radix(&hex, 16)
                    && let Some(decoded) = char::from_u32(codepoint)
                {
                    out.push(decoded);
                } else {
                    out.push_str("\\u{");
                    out.push_str(&hex);
                    out.push('}');
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    out
}
