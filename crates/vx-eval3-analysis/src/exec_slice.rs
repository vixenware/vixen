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
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecValue {
    Int(i64),
    Pair(Box<ExecValue>, Box<ExecValue>),
    Callable(CallableValue),
    LazyPoison(String),
    PendingRuntimeBoundaries(Vec<RuntimeNodeId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallableValue {
    AddConst(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    pub entry_result: ExecValue,
    pub body_results: BTreeMap<BodyId, ExecValue>,
    pub call_counts: BTreeMap<(BodyId, ExprId), usize>,
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
}

pub fn lower_executable_program(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) -> Result<ExecutableProgram, ExecSliceError> {
    let expr_nodes = expr_nodes_by_id(bridge);
    let fn_by_body = body_nodes_by_id(bridge)?;
    let runtime_nodes = executable_runtime_nodes(analysis);
    let mut bodies = BTreeMap::new();

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

pub fn eval_executable_program(program: &ExecutableProgram) -> Result<EvalOutcome, ExecSliceError> {
    let mut body_results = BTreeMap::new();
    let mut call_counts = BTreeMap::<(BodyId, ExprId), usize>::new();
    let entry = eval_body(
        program,
        program.entry,
        vec![],
        &mut body_results,
        &mut call_counts,
    )?;
    Ok(EvalOutcome {
        entry_result: entry,
        body_results,
        call_counts,
    })
}

fn eval_body(
    program: &ExecutableProgram,
    body_id: BodyId,
    args: Vec<ExecValue>,
    body_results: &mut BTreeMap<BodyId, ExecValue>,
    call_counts: &mut BTreeMap<(BodyId, ExprId), usize>,
) -> Result<ExecValue, ExecSliceError> {
    let body = program
        .bodies
        .get(&body_id)
        .ok_or(ExecSliceError::MissingBody(body_id))?;

    let mut slots = BTreeMap::<SlotId, ExecValue>::new();
    for (index, slot) in body.params.iter().enumerate() {
        let value = args.get(index).cloned().ok_or_else(|| {
            ExecSliceError::TypeMismatch(format!("missing arg {index} for body {}", body.id.0))
        })?;
        slots.insert(*slot, value);
    }

    for op in &body.ops {
        match op {
            KernelOp::AssignConstInt { dst, value, .. } => {
                slots.insert(*dst, ExecValue::Int(*value));
            }
            KernelOp::AssignAlias { dst, value, .. } => {
                let value = resolve_operand(value, &slots)?;
                slots.insert(*dst, value);
            }
            KernelOp::AssignLazyPoison { dst, label, .. } => {
                slots.insert(*dst, ExecValue::LazyPoison(label.clone()));
            }
            KernelOp::AssignCallableAddConst { dst, addend, .. } => {
                let addend = resolve_operand(addend, &slots)?;
                let addend_int = as_int(&addend, "callable addend")?;
                slots.insert(
                    *dst,
                    ExecValue::Callable(CallableValue::AddConst(addend_int)),
                );
            }
            KernelOp::ApplyCallable {
                dst, callable, arg, ..
            } => {
                let callable = resolve_operand(callable, &slots)?;
                let arg = resolve_operand(arg, &slots)?;
                let result = apply_callable(&callable, arg)?;
                slots.insert(*dst, result);
            }
            KernelOp::MakePair {
                dst, first, second, ..
            } => {
                let first = resolve_operand(first, &slots)?;
                let second = resolve_operand(second, &slots)?;
                slots.insert(*dst, ExecValue::Pair(Box::new(first), Box::new(second)));
            }
            KernelOp::ProjectIndex {
                dst, pair, index, ..
            } => {
                let pair = resolve_operand(pair, &slots)?;
                let projected = match pair {
                    ExecValue::Pair(first, second) => match index {
                        0 => *first,
                        1 => *second,
                        _ => return Err(ExecSliceError::InvalidPairIndex(*index)),
                    },
                    other => {
                        return Err(ExecSliceError::TypeMismatch(format!(
                            "project expects pair, got {:?}",
                            other
                        )));
                    }
                };
                slots.insert(*dst, projected);
            }
            KernelOp::Add { dst, lhs, rhs, .. } => {
                let lhs = as_int(&resolve_operand(lhs, &slots)?, "add lhs")?;
                let rhs = as_int(&resolve_operand(rhs, &slots)?, "add rhs")?;
                slots.insert(*dst, ExecValue::Int(lhs + rhs));
            }
            KernelOp::CallBody {
                dst,
                source,
                callee,
                args,
            } => {
                let arg_values = args
                    .iter()
                    .map(|arg| resolve_operand(arg, &slots))
                    .collect::<Result<Vec<_>, _>>()?;
                *call_counts.entry((source.body, source.expr)).or_insert(0) += 1;
                let value = eval_body(program, *callee, arg_values, body_results, call_counts)?;
                slots.insert(*dst, value);
            }
            KernelOp::AwaitRuntime { dst, node, .. } => {
                slots.insert(*dst, ExecValue::PendingRuntimeBoundaries(vec![*node]));
            }
        }
    }

    let result = match &body.result_source {
        ResultSource::Slot(slot) => slots
            .get(slot)
            .cloned()
            .ok_or(ExecSliceError::MissingSlot(*slot))?,
        ResultSource::PendingRuntimeBoundary => {
            ExecValue::PendingRuntimeBoundaries(body.runtime_boundary_extractions.clone())
        }
    };
    body_results.insert(body_id, result.clone());
    Ok(result)
}

fn resolve_operand(
    operand: &Operand,
    slots: &BTreeMap<SlotId, ExecValue>,
) -> Result<ExecValue, ExecSliceError> {
    match operand {
        Operand::Slot(slot) => slots
            .get(slot)
            .cloned()
            .ok_or(ExecSliceError::MissingSlot(*slot)),
        Operand::Int(value) => Ok(ExecValue::Int(*value)),
    }
}

fn as_int(value: &ExecValue, context: &str) -> Result<i64, ExecSliceError> {
    match value {
        ExecValue::Int(int) => Ok(*int),
        other => Err(ExecSliceError::TypeMismatch(format!(
            "{context} expected int, got {:?}",
            other
        ))),
    }
}

fn apply_callable(callable: &ExecValue, arg: ExecValue) -> Result<ExecValue, ExecSliceError> {
    let arg = as_int(&arg, "call arg")?;
    match callable {
        ExecValue::Callable(CallableValue::AddConst(value)) => Ok(ExecValue::Int(arg + value)),
        other => Err(ExecSliceError::TypeMismatch(format!(
            "apply expects callable, got {:?}",
            other
        ))),
    }
}

fn expr_nodes_by_id(bridge: &SingleSourceBridge) -> HashMap<ExprId, Expr> {
    let mut span_to_expr = HashMap::<SourceSpan, ExprId>::new();
    for expr in &bridge.exprs {
        if expr.span.source == bridge.source_id {
            span_to_expr.insert(expr.span, expr.id);
        }
    }

    let mut by_id = HashMap::new();
    for expr in bridge
        .parsed
        .tree()
        .syntax()
        .descendants()
        .filter_map(Expr::cast)
    {
        let span = SourceSpan::new(bridge.source_id, expr.syntax().text_range());
        if let Some(expr_id) = span_to_expr.get(&span).copied() {
            by_id.insert(expr_id, expr);
        }
    }
    by_id
}

fn body_nodes_by_id(
    bridge: &SingleSourceBridge,
) -> Result<HashMap<BodyId, FnDecl>, ExecSliceError> {
    let mut body_by_span = HashMap::<SourceSpan, BodyId>::new();
    for body in &bridge.bodies {
        if body.span.source == bridge.source_id {
            body_by_span.insert(body.span, body.id);
        }
    }

    let mut fn_by_body = HashMap::new();
    for item in bridge.parsed.tree().items() {
        let vx_parser::Item::FnDecl(fn_decl) = item else {
            continue;
        };
        let span = SourceSpan::new(bridge.source_id, fn_decl.syntax().text_range());
        let Some(body_id) = body_by_span.get(&span).copied() else {
            continue;
        };
        fn_by_body.insert(body_id, fn_decl);
    }

    if fn_by_body.is_empty() {
        return Err(UnsupportedShape(
            "bridge has no lowerable function declarations".to_owned(),
        ));
    }
    Ok(fn_by_body)
}

fn executable_runtime_nodes(
    analysis: &AnalysisResult,
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
        nodes.insert(id, ExecutableRuntimeNode { id, kind, source });
    }
    nodes
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
    analyzed: &'a AnalyzedBody,
    bridge: &'a SingleSourceBridge,
    expr_nodes: &'a HashMap<ExprId, Expr>,
    entry_expr: ExprId,
    use_binding_by_expr: HashMap<ExprId, BridgeLocalBindingId>,
    binding_initializers: HashMap<BridgeLocalBindingId, ExprId>,
    slot_by_binding: HashMap<BridgeLocalBindingId, SlotId>,
    runtime_node_by_expr: HashMap<ExprId, RuntimeNodeId>,
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
                let stmt_span = SourceSpan::new(bridge.source_id, let_stmt.syntax().text_range());
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
                let span = SourceSpan::new(bridge.source_id, expr.syntax().text_range());
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
            if matches!(
                extracted.reason,
                crate::ExtractedNodeReason::RuntimeBoundary { .. }
            ) {
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
            analyzed,
            bridge,
            expr_nodes,
            entry_expr,
            use_binding_by_expr,
            binding_initializers,
            slot_by_binding,
            runtime_node_by_expr,
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
        let span = SourceSpan::new(self.bridge.source_id, expr.syntax().text_range());
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
                    let dst = self.alloc_temp();
                    self.ops.push(KernelOp::AwaitRuntime {
                        dst,
                        source: self.source_for(expr_id),
                        node,
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
}

fn is_param_name_ref(expr: &Expr, name: &str) -> bool {
    let Expr::NameRef(name_ref) = expr else {
        return false;
    };
    name_ref.token().is_some_and(|token| token.text() == name)
}
