use std::collections::{BTreeMap, BTreeSet, HashSet};

use vx_resolve::SourceSpan;

use crate::{
    AnalysisResult, AnalyzedBody, BridgeBody, BridgeExpr, BridgeExprKind, CaptureSlot,
    ClosureEnvLayout, ExtractedNode, ExtractedNodeReason, KernelEntrypoint, KernelId, KernelRegion,
    LayoutId, Observation, RuntimeBoundaryKind, RuntimeNode, RuntimeNodeId, SingleSourceBridge,
    SourceExpr, UseSiteId, boundary::classify_runtime_boundary,
};

pub fn build_minimal_analysis(bridge: &SingleSourceBridge) -> AnalysisResult {
    let mut bodies = bridge
        .bodies
        .iter()
        .map(|body| analyzed_body_shell(body, &bridge.exprs))
        .collect::<Vec<_>>();

    let kernels = bridge
        .bodies
        .iter()
        .map(|body| kernel_shell(body, &bridge.exprs))
        .collect::<Vec<_>>();

    let mut runtime_nodes = Vec::new();
    let mut next_runtime_node = 0u32;

    let mut boundary_refs = bridge
        .resolved_refs
        .iter()
        .filter_map(|resolved| {
            let kind = classify_runtime_boundary(bridge.source_id, resolved)?;
            let source = enclosing_expr_source(resolved.span, &bridge.exprs)?;
            Some((resolved.span, source, kind))
        })
        .collect::<Vec<_>>();

    boundary_refs.sort_by_key(|(span, source, kind)| {
        (
            u32::from(span.range.start()),
            source.body.0,
            source.expr.0,
            *kind as u8,
        )
    });

    for (_, source, kind) in boundary_refs {
        let node_id = RuntimeNodeId(next_runtime_node);
        next_runtime_node += 1;

        let runtime = match kind {
            RuntimeBoundaryKind::Exec => RuntimeNode::Exec {
                id: node_id,
                source: source.clone(),
            },
            RuntimeBoundaryKind::Discover => RuntimeNode::Discover {
                id: node_id,
                source: source.clone(),
            },
            RuntimeBoundaryKind::SharedKeyed => RuntimeNode::SharedKeyed {
                id: node_id,
                source: source.clone(),
                keyed: crate::SharedKeyedNode {
                    key_expr: source.clone(),
                    request_expr: source.clone(),
                    join_expr: source.clone(),
                },
            },
        };
        runtime_nodes.push(runtime);

        if let Some(body) = bodies.iter_mut().find(|body| body.id == source.body) {
            body.extracted_nodes.push(ExtractedNode {
                id: node_id,
                source,
                reason: ExtractedNodeReason::RuntimeBoundary { kind },
            });
        }
    }

    seed_use_observations(&mut bodies, bridge);
    let closure_layouts = seed_closure_layouts(bridge);

    AnalysisResult {
        bodies,
        kernels,
        runtime_nodes,
        closure_layouts,
        frame_layouts: vec![],
    }
}

fn seed_use_observations(bodies: &mut [AnalyzedBody], bridge: &SingleSourceBridge) {
    let mut seeds = Vec::new();

    for resolved in &bridge.resolved_refs {
        let Some(_kind) = classify_runtime_boundary(bridge.source_id, resolved) else {
            continue;
        };
        let obs = Observation::NeedCallable;
        let Some(source) =
            enclosing_expr_source_with_kind(resolved.span, &bridge.exprs, BridgeExprKind::Call)
        else {
            continue;
        };
        seeds.push((source.body, source.expr, obs));
    }

    for expr in bridge
        .exprs
        .iter()
        .filter(|expr| expr.kind == BridgeExprKind::Field)
    {
        let Some(body) = expr.body else {
            continue;
        };
        seeds.push((body, expr.id, Observation::NeedValue));
    }

    seeds.sort_by_key(|(body, expr, obs)| {
        let obs_rank = match obs {
            Observation::NoObs => 0u8,
            Observation::NeedValue => 1,
            Observation::NeedCallable => 2,
            Observation::NeedProduct(_) => 3,
        };
        (body.0, expr.0, obs_rank)
    });
    seeds.dedup();

    for body in bodies {
        let mut next_id = 0u32;
        for (_, _, obs) in seeds
            .iter()
            .filter(|(seed_body, _, _)| *seed_body == body.id)
        {
            body.use_observations
                .insert(UseSiteId(next_id), obs.clone());
            next_id += 1;
        }
    }
}

fn seed_closure_layouts(bridge: &SingleSourceBridge) -> Vec<ClosureEnvLayout> {
    let mut closure_exprs = bridge
        .exprs
        .iter()
        .filter(|expr| expr.kind == BridgeExprKind::Closure)
        .filter_map(|expr| Some((expr.body?, expr)))
        .collect::<Vec<_>>();
    closure_exprs.sort_by_key(|(body_id, expr)| (body_id.0, expr.id.0));

    let mut layouts = Vec::new();
    for (index, (body_id, closure_expr)) in closure_exprs.into_iter().enumerate() {
        let Some(body) = bridge.bodies.iter().find(|body| body.id == body_id) else {
            continue;
        };

        let nested_closure_ranges = bridge
            .exprs
            .iter()
            .filter(|expr| expr.kind == BridgeExprKind::Closure)
            .filter(|expr| expr.id != closure_expr.id)
            .map(|expr| expr.span.range)
            .filter(|range| closure_expr.span.range.contains_range(*range))
            .collect::<Vec<_>>();

        let body_param_spans = body
            .params
            .iter()
            .map(|param| param.span)
            .collect::<HashSet<_>>();

        let mut candidates = bridge
            .resolved_refs
            .iter()
            .filter(|resolved| closure_expr.span.range.contains_range(resolved.span.range))
            .filter(|resolved| {
                !nested_closure_ranges
                    .iter()
                    .any(|range| range.contains_range(resolved.span.range))
            })
            .filter_map(|resolved| {
                let target = resolved.target.as_ref().ok()?;
                if target.kind != vx_resolve::DefKind::Parameter {
                    return None;
                }
                if !body_param_spans.contains(&target.name_span) {
                    return None;
                }

                let source = enclosing_expr_source(resolved.span, &bridge.exprs)?;
                Some((target.name.clone(), target.name_span, source))
            })
            .collect::<Vec<_>>();

        candidates.sort_by_key(|(name, span, source)| {
            (
                u32::from(span.range.start()),
                u32::from(span.range.end()),
                name.clone(),
                source.expr.0,
            )
        });
        candidates.dedup_by(|lhs, rhs| lhs.1 == rhs.1);

        let captures = candidates
            .into_iter()
            .map(|(name, _, source)| CaptureSlot { name, source })
            .collect();

        layouts.push(ClosureEnvLayout {
            id: LayoutId(index as u32),
            captures,
        });
    }

    layouts
}

fn analyzed_body_shell(body: &BridgeBody, exprs: &[BridgeExpr]) -> AnalyzedBody {
    let first_expr = exprs
        .iter()
        .find(|expr| expr.body == Some(body.id))
        .map(|expr| expr.id)
        .unwrap_or(crate::ExprId(0));

    AnalyzedBody {
        id: body.id,
        result_observation: Observation::NeedValue,
        use_observations: BTreeMap::new(),
        extracted_nodes: vec![],
        kernel_entries: vec![KernelEntrypoint {
            body: body.id,
            source: SourceExpr {
                body: body.id,
                expr: first_expr,
            },
            kernel: KernelId(body.id.0),
            demanded_result: Observation::NeedValue,
        }],
    }
}

fn kernel_shell(body: &BridgeBody, exprs: &[BridgeExpr]) -> KernelRegion {
    let body_exprs = exprs
        .iter()
        .filter(|expr| expr.body == Some(body.id))
        .map(|expr| expr.id)
        .collect::<BTreeSet<_>>();

    KernelRegion {
        id: KernelId(body.id.0),
        body: body.id,
        exprs: body_exprs,
        params: vec![],
        result_obs: Observation::NeedValue,
    }
}

fn enclosing_expr_source(span: SourceSpan, exprs: &[BridgeExpr]) -> Option<SourceExpr> {
    enclosing_expr_source_with_kind(span, exprs, BridgeExprKind::Other)
}

fn enclosing_expr_source_with_kind(
    span: SourceSpan,
    exprs: &[BridgeExpr],
    kind: BridgeExprKind,
) -> Option<SourceExpr> {
    exprs
        .iter()
        .filter(|expr| expr.span.range.contains_range(span.range))
        .filter(|expr| kind == BridgeExprKind::Other || expr.kind == kind)
        .min_by_key(|expr| {
            let len = u32::from(expr.span.range.end()) - u32::from(expr.span.range.start());
            (len, expr.id.0)
        })
        .and_then(|expr| {
            Some(SourceExpr {
                body: expr.body?,
                expr: expr.id,
            })
        })
}
