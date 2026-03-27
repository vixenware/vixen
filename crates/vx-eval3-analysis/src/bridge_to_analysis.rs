use std::collections::{BTreeMap, BTreeSet};

use vx_resolve::SourceSpan;

use crate::{
    AnalysisResult, AnalyzedBody, BridgeBody, BridgeExpr, ExtractedNode, ExtractedNodeReason,
    KernelEntrypoint, KernelId, KernelRegion, Observation, RuntimeBoundaryKind, RuntimeNode,
    RuntimeNodeId, SingleSourceBridge, SourceExpr,
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
            let kind = match resolved.name.as_str() {
                "exec" => RuntimeBoundaryKind::Exec,
                "discover" => RuntimeBoundaryKind::Discover,
                _ => return None,
            };
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

    AnalysisResult {
        bodies,
        kernels,
        runtime_nodes,
        closure_layouts: vec![],
        frame_layouts: vec![],
    }
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
    exprs
        .iter()
        .filter(|expr| expr.span.range.contains_range(span.range))
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
