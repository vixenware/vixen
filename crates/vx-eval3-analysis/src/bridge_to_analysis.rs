use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use vx_resolve::{DefId, SourceSpan};

use crate::{
    AnalysisResult, AnalyzedBody, BodyId, BridgeBody, BridgeExpr, BridgeExprKind,
    BridgeLocalBindingId, BridgeLocalBindingKind, CaptureSlot, ClosureEnvLayout, ExprId,
    ExtractedNode, ExtractedNodeReason, KernelEntrypoint, KernelId, KernelRegion, LayoutId,
    Observation, RuntimeBoundaryKind, RuntimeNode, RuntimeNodeId, SingleSourceBridge, SourceExpr,
    UseSiteId,
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
        .call_sites
        .iter()
        .filter_map(|site| match site.kind {
            crate::BridgeCallSiteKind::StdlibBoundary(kind) => Some((
                SourceExpr {
                    body: site.body,
                    expr: site.expr,
                },
                kind,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    boundary_refs.sort_by_key(|(source, kind)| (source.body.0, source.expr.0, *kind as u8));

    for (source, kind) in boundary_refs {
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

    seed_structural_sharing(
        &mut bodies,
        &mut runtime_nodes,
        &mut next_runtime_node,
        bridge,
    );
    seed_lazy_product_fields(
        &mut bodies,
        &mut runtime_nodes,
        &mut next_runtime_node,
        bridge,
    );
    let closure_layouts = seed_closure_layouts(bridge);
    seed_callee_targets(&mut bodies, bridge);
    seed_source_local_callee_origins(&mut bodies, bridge);
    seed_source_local_call_edges(&mut bodies);
    seed_callee_closure_layout_links(&mut bodies, &closure_layouts);
    let propagated_result_demands = seed_first_result_propagation_step(&bodies);
    seed_body_result_observations(&mut bodies, bridge, &propagated_result_demands);
    seed_use_observations(&mut bodies, bridge);

    AnalysisResult {
        bodies,
        kernels,
        runtime_nodes,
        closure_layouts,
        frame_layouts: vec![],
        propagated_result_demands,
    }
}

fn seed_body_result_observations(
    bodies: &mut [AnalyzedBody],
    bridge: &SingleSourceBridge,
    propagated: &BTreeMap<crate::ResultDemandCallee, Observation>,
) {
    for body in bodies.iter_mut() {
        body.result_observation = Observation::NoObs;
    }

    let entry_ids = {
        let mains = bridge
            .bodies
            .iter()
            .filter(|body| body.name == "main")
            .map(|body| body.id)
            .collect::<Vec<_>>();
        if mains.is_empty() {
            bridge
                .bodies
                .first()
                .map(|body| vec![body.id])
                .unwrap_or_default()
        } else {
            mains
        }
    };
    for entry_id in entry_ids {
        if let Some(body) = bodies.iter_mut().find(|body| body.id == entry_id) {
            body.result_observation = Observation::NeedValue;
        }
    }

    for (callee, obs) in propagated {
        let crate::ResultDemandCallee::TopLevelBody(callee_body) = callee else {
            continue;
        };
        if let Some(body) = bodies.iter_mut().find(|body| body.id == *callee_body) {
            body.result_observation = obs.clone();
        }
    }
}

fn seed_source_local_callee_origins(bodies: &mut [AnalyzedBody], bridge: &SingleSourceBridge) {
    let top_level_by_def = bridge
        .bodies
        .iter()
        .filter_map(|body| Some((body.def_id?, body.id)))
        .collect::<HashMap<_, _>>();
    let local_binding_by_def = bridge
        .local_bindings
        .iter()
        .map(|binding| (binding.def_id, binding.id))
        .collect::<HashMap<_, _>>();
    let local_closure_init_by_binding = bridge
        .local_closure_inits
        .iter()
        .map(|init| (init.binding, init.source.clone()))
        .collect::<HashMap<_, _>>();

    for body in bodies {
        let seeds = body
            .callee_targets
            .iter()
            .filter_map(|(expr, def_id)| {
                if let Some(origin_body) = top_level_by_def.get(def_id) {
                    return Some((
                        *expr,
                        crate::SourceLocalCalleeOrigin::TopLevelBody(*origin_body),
                    ));
                }
                if let Some(binding_id) = local_binding_by_def.get(def_id).copied()
                    && let Some(closure_source) =
                        local_closure_init_by_binding.get(&binding_id).cloned()
                {
                    return Some((
                        *expr,
                        crate::SourceLocalCalleeOrigin::LocalClosure(closure_source),
                    ));
                }
                if local_binding_by_def.contains_key(def_id) {
                    return Some((*expr, crate::SourceLocalCalleeOrigin::LocalDef(*def_id)));
                }
                None
            })
            .collect::<Vec<_>>();

        for (expr, origin) in seeds {
            body.callee_local_origins.insert(expr, origin);
        }
    }
}

fn seed_source_local_call_edges(bodies: &mut [AnalyzedBody]) {
    for body in bodies {
        let mut edges = body
            .callee_local_origins
            .iter()
            .map(|(call_expr, callee)| crate::SourceLocalCallEdge {
                call_expr: *call_expr,
                callee: callee.clone(),
            })
            .collect::<Vec<_>>();
        edges.sort_by_key(|edge| {
            let callee_rank = match &edge.callee {
                crate::SourceLocalCalleeOrigin::TopLevelBody(body) => (0u8, body.0, 0u32),
                crate::SourceLocalCalleeOrigin::LocalClosure(source_expr) => {
                    (1, source_expr.body.0, source_expr.expr.0)
                }
                crate::SourceLocalCalleeOrigin::LocalDef(DefId::Source { source, local }) => {
                    (2, source.0, *local)
                }
            };
            (edge.call_expr.0, callee_rank)
        });
        body.source_local_call_edges = edges;
    }
}

fn seed_callee_closure_layout_links(bodies: &mut [AnalyzedBody], layouts: &[ClosureEnvLayout]) {
    let layout_by_source = layouts
        .iter()
        .map(|layout| (layout.source.clone(), layout.id))
        .collect::<HashMap<_, _>>();

    for body in bodies {
        let mut seeds = body
            .callee_local_origins
            .iter()
            .filter_map(|(call_expr, origin)| match origin {
                crate::SourceLocalCalleeOrigin::LocalClosure(source) => {
                    Some((*call_expr, *layout_by_source.get(source)?))
                }
                crate::SourceLocalCalleeOrigin::TopLevelBody(_)
                | crate::SourceLocalCalleeOrigin::LocalDef(_) => None,
            })
            .collect::<Vec<_>>();
        seeds.sort_by_key(|(expr, layout)| (expr.0, layout.0));
        seeds.dedup();

        for (expr, layout) in seeds {
            body.callee_closure_layouts.insert(expr, layout);
        }
    }
}

fn seed_first_result_propagation_step(
    bodies: &[AnalyzedBody],
) -> BTreeMap<crate::ResultDemandCallee, Observation> {
    let mut demands = BTreeMap::new();

    for body in bodies {
        for edge in &body.source_local_call_edges {
            match edge.callee {
                crate::SourceLocalCalleeOrigin::TopLevelBody(callee_body) => {
                    demands.insert(
                        crate::ResultDemandCallee::TopLevelBody(callee_body),
                        Observation::NeedValue,
                    );
                }
                crate::SourceLocalCalleeOrigin::LocalClosure(_) => {
                    let Some(layout_id) = body.callee_closure_layouts.get(&edge.call_expr).copied()
                    else {
                        continue;
                    };
                    demands.insert(
                        crate::ResultDemandCallee::ClosureLayout(layout_id),
                        Observation::NeedValue,
                    );
                }
                crate::SourceLocalCalleeOrigin::LocalDef(_) => {}
            }
        }
    }

    demands
}

fn seed_lazy_product_fields(
    bodies: &mut [AnalyzedBody],
    runtime_nodes: &mut Vec<RuntimeNode>,
    next_runtime_node: &mut u32,
    bridge: &SingleSourceBridge,
) {
    let binding_products = bridge
        .binding_products
        .iter()
        .map(|product| ((product.body, product.binding.as_str()), &product.fields))
        .collect::<HashMap<_, _>>();

    let mut seeds = bridge
        .projections
        .iter()
        .filter_map(|projection| {
            let binding = projection.base_binding.as_deref()?;
            let fields = binding_products.get(&(projection.body, binding))?;
            if fields.len() <= 1 || !fields.contains(&projection.field) {
                return None;
            }

            let sibling = fields
                .iter()
                .filter(|field| **field != projection.field)
                .min()
                .cloned()?;
            Some((projection.body, projection.base, sibling))
        })
        .collect::<Vec<_>>();
    seeds.sort_by_key(|(body, base, field)| (body.0, base.0, field.clone()));
    seeds.dedup();

    for (body_id, source_expr, sibling_field) in seeds {
        let node_id = RuntimeNodeId(*next_runtime_node);
        *next_runtime_node += 1;

        let source = SourceExpr {
            body: body_id,
            expr: source_expr,
        };
        runtime_nodes.push(RuntimeNode::Lazy {
            id: node_id,
            source: source.clone(),
        });

        if let Some(body) = bodies.iter_mut().find(|body| body.id == body_id) {
            body.extracted_nodes.push(ExtractedNode {
                id: node_id,
                source,
                reason: ExtractedNodeReason::LazyProductField {
                    field: sibling_field,
                },
            });
        }
    }
}

fn seed_structural_sharing(
    bodies: &mut [AnalyzedBody],
    runtime_nodes: &mut Vec<RuntimeNode>,
    next_runtime_node: &mut u32,
    bridge: &SingleSourceBridge,
) {
    let binding_by_id = bridge
        .local_bindings
        .iter()
        .map(|binding| (binding.id, binding))
        .collect::<HashMap<_, _>>();

    let mut refs_by_binding = HashMap::<(BodyId, BridgeLocalBindingId), Vec<ExprId>>::new();
    for usage in &bridge.local_uses {
        let Some(binding) = binding_by_id.get(&usage.binding) else {
            continue;
        };
        if binding.kind != BridgeLocalBindingKind::Let {
            continue;
        }
        refs_by_binding
            .entry((usage.body, usage.binding))
            .or_default()
            .push(usage.expr);
    }

    let mut seeds = refs_by_binding
        .into_iter()
        .filter_map(|((body, binding_id), mut refs)| {
            if refs.len() <= 1 {
                return None;
            }
            refs.sort_by_key(|expr| expr.0);
            refs.dedup();
            if refs.len() <= 1 {
                return None;
            }
            let def_id = binding_by_id.get(&binding_id)?.def_id;
            let source = SourceExpr {
                body,
                expr: refs[0],
            };
            Some((body, binding_id, def_id, source))
        })
        .collect::<Vec<_>>();

    seeds.sort_by_key(|(body, _, def_id, source)| {
        let (def_source, local) = match *def_id {
            DefId::Source { source, local } => (source, local),
        };
        (body.0, def_source.0, local, source.expr.0)
    });

    for (body_id, _, _, source) in seeds {
        let node_id = RuntimeNodeId(*next_runtime_node);
        *next_runtime_node += 1;

        runtime_nodes.push(RuntimeNode::Lazy {
            id: node_id,
            source: source.clone(),
        });

        if let Some(body) = bodies.iter_mut().find(|body| body.id == body_id) {
            body.extracted_nodes.push(ExtractedNode {
                id: node_id,
                source,
                reason: ExtractedNodeReason::SharedAcrossUses,
            });
        }
    }
}

fn seed_use_observations(bodies: &mut [AnalyzedBody], bridge: &SingleSourceBridge) {
    let mut seeds = Vec::new();

    for site in &bridge.call_sites {
        match site.kind {
            crate::BridgeCallSiteKind::DirectNamedFunction
            | crate::BridgeCallSiteKind::LocalCallableBinding
            | crate::BridgeCallSiteKind::StdlibBoundary(_) => {
                seeds.push((site.body, site.expr, Observation::NeedCallable));
            }
            crate::BridgeCallSiteKind::Other => {}
        }
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

fn seed_callee_targets(bodies: &mut [AnalyzedBody], bridge: &SingleSourceBridge) {
    let mut seeds = bridge
        .call_sites
        .iter()
        .filter_map(|site| {
            let target = site.target?;
            match site.kind {
                crate::BridgeCallSiteKind::DirectNamedFunction
                | crate::BridgeCallSiteKind::LocalCallableBinding => {
                    Some((site.body, site.expr, target))
                }
                crate::BridgeCallSiteKind::StdlibBoundary(_) | crate::BridgeCallSiteKind::Other => {
                    None
                }
            }
        })
        .collect::<Vec<_>>();
    seeds.sort_by_key(|(body, expr, target)| {
        let (source, local) = match target {
            DefId::Source { source, local } => (source.0, *local),
        };
        (body.0, expr.0, source, local)
    });
    seeds.dedup();

    for body in bodies {
        for (_, expr, target) in seeds
            .iter()
            .filter(|(seed_body, _, _)| *seed_body == body.id)
        {
            body.callee_targets.insert(*expr, *target);
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
            source: SourceExpr {
                body: body_id,
                expr: closure_expr.id,
            },
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
        callee_targets: BTreeMap::new(),
        callee_local_origins: BTreeMap::new(),
        source_local_call_edges: vec![],
        callee_closure_layouts: BTreeMap::new(),
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
