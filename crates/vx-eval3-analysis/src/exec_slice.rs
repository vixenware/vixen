use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use vx_parser::{AstNode, Expr, FnDecl, Stmt, SyntaxKind};
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelOp {
    AssignConstInt {
        dst: SlotId,
        source: SourceExpr,
        value: i64,
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
    Closure {
        source: SourceExpr,
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

struct BodyLowering<'a> {
    body: BodyId,
    body_source_id: vx_resolve::SourceId,
    analyzed: &'a AnalyzedBody,
    bridge: &'a SingleSourceBridge,
    expr_nodes: &'a HashMap<ExprId, Expr>,
    entry_expr: ExprId,
    use_binding_by_expr: HashMap<ExprId, BridgeLocalBindingId>,
    binding_initializers: HashMap<BridgeLocalBindingId, ExprId>,
    slot_by_binding: HashMap<BridgeLocalBindingId, SlotId>,
    runtime_node_by_expr: HashMap<ExprId, RuntimeNodeId>,
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

        let mut slot_by_binding = HashMap::new();
        let mut next_slot = 0u32;
        for binding in &body_bindings {
            slot_by_binding.insert(binding.id, SlotId(next_slot));
            next_slot += 1;
        }

        let mut params = Vec::new();
        for binding in body_bindings
            .iter()
            .filter(|binding| binding.kind == BridgeLocalBindingKind::Parameter)
        {
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
        for extracted in &analyzed.extracted_nodes {
            if let crate::ExtractedNodeReason::RuntimeBoundary { kind: _ } = extracted.reason {
                runtime_node_by_expr.insert(extracted.source.expr, extracted.id);
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
            binding_initializers,
            slot_by_binding,
            runtime_node_by_expr,
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
                if where_expr.keywords().is_some() {
                    return Err(UnsupportedShape(
                        "where keywords/observer lowering is not supported in executable slice"
                            .to_owned(),
                    ));
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
                if token.kind() != SyntaxKind::INT {
                    return Err(UnsupportedShape(format!(
                        "only INT literals supported, got `{}`",
                        token.text()
                    )));
                }
                let value = token
                    .text()
                    .replace('_', "")
                    .parse::<i64>()
                    .map_err(|_| UnsupportedShape(format!("invalid int `{}`", token.text())))?;
                Ok(Operand::Int(value))
            }
            Expr::NameRef(_) => {
                let binding = self.use_binding_by_expr.get(&expr_id).copied().ok_or(
                    ExecSliceError::MissingBindingForUse(self.source_for(expr_id)),
                )?;
                let slot = self.materialize_binding(binding)?;
                Ok(Operand::Slot(slot))
            }
            Expr::Binary(binary) => {
                let is_plus = binary
                    .syntax()
                    .children_with_tokens()
                    .filter_map(|child| child.into_token())
                    .any(|token| token.kind() == SyntaxKind::PLUS);
                if !is_plus {
                    return Err(UnsupportedShape(format!(
                        "only + binary is supported in exec slice (expr {})",
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
                self.ops.push(KernelOp::Add {
                    dst,
                    source: self.source_for(expr_id),
                    lhs,
                    rhs,
                });
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
            Expr::Field(field) => {
                let field_name = field
                    .field_name()
                    .ok_or_else(|| UnsupportedShape("field name missing".to_owned()))?
                    .text()
                    .to_owned();
                let index = field_name
                    .strip_prefix('_')
                    .ok_or_else(|| UnsupportedShape(format!("unsupported field `{field_name}`")))?
                    .parse::<usize>()
                    .map_err(|_| UnsupportedShape(format!("invalid field `{field_name}`")))?;

                let receiver = field
                    .receiver()
                    .ok_or_else(|| UnsupportedShape("field receiver missing".to_owned()))?;

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
                Ok(Operand::Slot(dst))
            }
            Expr::Closure(closure) => {
                let mut params = closure.parameters();
                let Some(param) = params.next() else {
                    return Err(UnsupportedShape(
                        "closure requires one parameter".to_owned(),
                    ));
                };
                if params.next().is_some() {
                    return Err(UnsupportedShape(
                        "only single-parameter closure supported".to_owned(),
                    ));
                }
                let param_name = param
                    .name()
                    .ok_or_else(|| UnsupportedShape("closure parameter missing".to_owned()))?
                    .text()
                    .to_owned();
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
                let addend = if is_param_name_ref(&lhs, &param_name) {
                    self.lower_expr(
                        self.expr_id(&rhs)
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(body_id)))?,
                    )?
                } else if is_param_name_ref(&rhs, &param_name) {
                    self.lower_expr(
                        self.expr_id(&lhs)
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(body_id)))?,
                    )?
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
                    });
                    self.awaited_runtime_nodes.insert(node);
                    return Ok(Operand::Slot(dst));
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

                let source = self.source_for(expr_id);
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
                let callee_expr = call
                    .callee()
                    .ok_or_else(|| UnsupportedShape("call has no callee".to_owned()))?;
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
                Err(UnsupportedShape(_)) => {
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

        if let Expr::Closure(_) = expr {
            let expr_id = self.expr_id(expr).ok_or(ExecSliceError::MissingExpr(
                self.source_for(self.entry_expr),
            ))?;
            return Ok(RuntimeBoundaryInputSpec::Closure {
                source: self.source_for(expr_id),
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
}

fn is_param_name_ref(expr: &Expr, name: &str) -> bool {
    let Expr::NameRef(name_ref) = expr else {
        return false;
    };
    name_ref.token().is_some_and(|token| token.text() == name)
}

fn normalize_string_literal_token(token_text: &str) -> String {
    token_text
        .strip_prefix('"')
        .and_then(|stripped| stripped.strip_suffix('"'))
        .unwrap_or(token_text)
        .to_owned()
}
