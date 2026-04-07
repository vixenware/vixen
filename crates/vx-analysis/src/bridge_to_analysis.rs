use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use vx_resolve::{DefId, SourceSpan};

use crate::{
    AnalysisResult, AnalyzedBody, BodyId, BridgeBody, BridgeExpr, BridgeExprKind,
    BridgeLocalBindingId, BridgeLocalBindingKind, CaptureSlot, ClosureEnvLayout, ExprId,
    ExtractedNode, ExtractedNodeReason, FieldKey, KernelEntrypoint, KernelId, KernelRegion,
    LayoutId, Observation, RuntimeBoundaryKind, RuntimeNode, RuntimeNodeId, SingleSourceBridge,
    SourceExpr, UseSiteId,
};

pub fn build_minimal_analysis(bridge: &SingleSourceBridge) -> AnalysisResult {
    let mut bodies = bridge
        .bodies
        .iter()
        .map(|body| analyzed_body_shell(body, &bridge.exprs))
        .collect::<Vec<_>>();

    // Seed extracted nodes onto bodies BEFORE building kernels, so that
    // kernel expr sets can exclude extracted expressions.
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

    // Build kernels AFTER extracted nodes are seeded, so we can exclude
    // extracted expressions from each kernel's expr set.
    let extracted_exprs_by_body = {
        let mut map: BTreeMap<BodyId, BTreeSet<ExprId>> = BTreeMap::new();
        for body in &bodies {
            for extracted in &body.extracted_nodes {
                map.entry(body.id)
                    .or_default()
                    .insert(extracted.source.expr);
            }
        }
        map
    };

    let mut kernels = bridge
        .bodies
        .iter()
        .map(|body| kernel_shell(body, bridge, extracted_exprs_by_body.get(&body.id)))
        .collect::<Vec<_>>();

    let mut closure_layouts = seed_closure_layouts(bridge);
    seed_callee_targets(&mut bodies, bridge);
    seed_source_local_callee_origins(&mut bodies, bridge);
    seed_source_local_call_edges(&mut bodies);
    seed_callee_closure_layout_links(&mut bodies, &closure_layouts);
    seed_use_observations(&mut bodies, bridge);
    apply_one_step_local_call_demands(&mut bodies, &mut kernels, &mut closure_layouts, bridge);

    AnalysisResult {
        bodies,
        kernels,
        runtime_nodes,
        closure_layouts,
        frame_layouts: vec![],
    }
}

fn apply_one_step_local_call_demands(
    bodies: &mut [AnalyzedBody],
    kernels: &mut [KernelRegion],
    layouts: &mut [ClosureEnvLayout],
    bridge: &SingleSourceBridge,
) {
    for body in bodies.iter_mut() {
        body.result_observation = Observation::NoObs;
        body.incoming_use_observation = Observation::NoObs;
    }
    for layout in layouts.iter_mut() {
        layout.incoming_use_observation = Observation::NoObs;
        for capture in &mut layout.captures {
            capture.incoming_use_observation = Observation::NoObs;
        }
    }

    let mut mains = bridge
        .bodies
        .iter()
        .filter(|body| body.name == "main" && body.span.source == bridge.source_id)
        .map(|body| body.id)
        .collect::<Vec<_>>();
    if mains.is_empty() {
        mains = bridge
            .bodies
            .iter()
            .filter(|body| body.name == "main")
            .map(|body| body.id)
            .collect::<Vec<_>>();
    }
    mains.sort_by_key(|id| id.0);
    mains.dedup();
    let use_transitive_local_call_propagation = mains.is_empty();
    let entry_ids = if !mains.is_empty() {
        mains
    } else {
        let mut builds = bridge
            .bodies
            .iter()
            .filter(|body| body.name == "build" && body.span.source == bridge.source_id)
            .map(|body| body.id)
            .collect::<Vec<_>>();
        if builds.is_empty() {
            builds = bridge
                .bodies
                .iter()
                .filter(|body| body.name == "build")
                .map(|body| body.id)
                .collect::<Vec<_>>();
        }
        builds.sort_by_key(|body_id| body_id.0);
        builds.dedup();
        if !builds.is_empty() {
            builds
        } else {
            let mut incoming = BTreeSet::new();
            for body in bodies.iter() {
                for edge in &body.source_local_call_edges {
                    if let crate::SourceLocalCalleeOrigin::TopLevelBody(callee_body) = edge.callee {
                        incoming.insert(callee_body);
                    }
                }
            }
            let mut roots = bodies
                .iter()
                .map(|body| body.id)
                .filter(|body_id| !incoming.contains(body_id))
                .collect::<Vec<_>>();
            roots.sort_by_key(|body_id| body_id.0);
            if let Some(root) = roots.first().copied() {
                vec![root]
            } else {
                bridge
                    .bodies
                    .first()
                    .map(|body| vec![body.id])
                    .unwrap_or_default()
            }
        }
    };
    for entry_id in &entry_ids {
        if let Some(body) = bodies.iter_mut().find(|body| body.id == *entry_id) {
            body.result_observation = Observation::NeedValue;
        }
    }

    let mut demanded_bodies = BTreeSet::new();
    let mut demanded_layouts = BTreeSet::new();
    let mut visit_queue = entry_ids;
    let mut visited = BTreeSet::new();
    while let Some(caller_id) = visit_queue.pop() {
        if !visited.insert(caller_id) {
            continue;
        }
        let Some(caller) = bodies.iter().find(|body| body.id == caller_id) else {
            continue;
        };
        for edge in &caller.source_local_call_edges {
            match edge.callee {
                crate::SourceLocalCalleeOrigin::TopLevelBody(callee_body) => {
                    demanded_bodies.insert(callee_body);
                    if use_transitive_local_call_propagation {
                        visit_queue.push(callee_body);
                    }
                }
                crate::SourceLocalCalleeOrigin::LocalClosure(_) => {
                    if let Some(layout_id) = caller.callee_closure_layouts.get(&edge.call_expr) {
                        demanded_layouts.insert(*layout_id);
                    }
                }
                crate::SourceLocalCalleeOrigin::LocalDef(_) => {}
            }
        }
    }

    for callee_body in demanded_bodies {
        if let Some(body) = bodies.iter_mut().find(|body| body.id == callee_body) {
            body.result_observation = Observation::NeedValue;
            if body.incoming_use_observation == Observation::NoObs {
                body.incoming_use_observation = Observation::NeedValue;
                let next_use_site = body
                    .use_observations
                    .keys()
                    .map(|id| id.0)
                    .max()
                    .map(|max| max + 1)
                    .unwrap_or(0);
                body.use_observations
                    .insert(UseSiteId(next_use_site), Observation::NeedValue);
            }
        }
    }

    for layout_id in demanded_layouts {
        if let Some(layout) = layouts.iter_mut().find(|layout| layout.id == layout_id) {
            layout.incoming_use_observation = Observation::NeedValue;
            for capture in &mut layout.captures {
                capture.incoming_use_observation = Observation::NeedValue;
            }
        }
    }

    seed_kernel_demands(bodies, kernels);
}

fn seed_kernel_demands(bodies: &mut [AnalyzedBody], kernels: &mut [KernelRegion]) {
    for body in bodies {
        for entry in &mut body.kernel_entries {
            entry.demanded_result = body.result_observation.clone();
        }

        for kernel in kernels.iter_mut().filter(|kernel| kernel.body == body.id) {
            kernel.result_obs = body.result_observation.clone();
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

fn seed_lazy_product_fields(
    bodies: &mut [AnalyzedBody],
    runtime_nodes: &mut Vec<RuntimeNode>,
    next_runtime_node: &mut u32,
    bridge: &SingleSourceBridge,
) {
    // Index: binding name → binding ID (for resolving BridgeBindingProduct)
    let binding_id_by_name: HashMap<(BodyId, &str), BridgeLocalBindingId> = bridge
        .local_bindings
        .iter()
        .map(|b| ((b.body, b.name.as_str()), b.id))
        .collect();

    // Index: ExprId → binding ID (for resolving projection bases)
    let use_by_expr: HashMap<ExprId, BridgeLocalBindingId> = bridge
        .local_uses
        .iter()
        .map(|u| (u.expr, u.binding))
        .collect();

    // Direct binding → product field keys (keyed by binding ID)
    let binding_products: HashMap<BridgeLocalBindingId, Vec<FieldKey>> = bridge
        .binding_products
        .iter()
        .filter_map(|product| {
            let id = binding_id_by_name.get(&(product.body, product.binding.as_str()))?;
            Some((*id, product.fields.clone()))
        })
        .collect();

    // Products by (body, expr) → field keys with sub-expression ExprIds
    let products_by_expr: HashMap<(BodyId, ExprId), &Vec<(FieldKey, Option<ExprId>)>> = bridge
        .products
        .iter()
        .map(|product| ((product.body, product.expr), &product.fields))
        .collect();

    // Projections indexed by expr
    let projection_by_expr: HashMap<(BodyId, ExprId), &crate::BridgeProjection> = bridge
        .projections
        .iter()
        .map(|p| ((p.body, p.expr), p))
        .collect();

    // Derive binding → product field keys by following projection chains.
    // If `let inner = outer._0` and `outer._0` sub-expression is a product,
    // then `inner` is effectively that product.
    let mut all_binding_products: HashMap<BridgeLocalBindingId, Vec<FieldKey>> = binding_products;

    // Follow one level of projection: let X = Y.field
    for binding in &bridge.local_bindings {
        if binding.kind != crate::BridgeLocalBindingKind::Let {
            continue;
        }
        let Some(init_expr) = binding.init_expr else {
            continue;
        };
        // Is the init expression a projection?
        let Some(proj) = projection_by_expr.get(&(binding.body, init_expr)) else {
            continue;
        };
        // Resolve base binding by ExprId (sound under shadowing)
        let Some(&base_binding_id) = use_by_expr.get(&proj.base) else {
            continue;
        };
        let Some(base_binding) = bridge
            .local_bindings
            .iter()
            .find(|b| b.id == base_binding_id)
        else {
            continue;
        };
        let Some(base_init) = base_binding.init_expr else {
            continue;
        };
        let Some(product_fields) = products_by_expr.get(&(binding.body, base_init)) else {
            continue;
        };
        // Find the sub-expression for the projected field
        let Some(sub_expr) =
            product_fields.iter().find_map(
                |(key, expr_id)| {
                    if *key == proj.field { *expr_id } else { None }
                },
            )
        else {
            continue;
        };
        // Is that sub-expression also a product?
        if let Some(sub_product) = products_by_expr.get(&(binding.body, sub_expr)) {
            let sub_keys: Vec<FieldKey> = sub_product
                .iter()
                .map(|(k, _): &(FieldKey, _)| k.clone())
                .collect();
            if sub_keys.len() > 1 {
                all_binding_products.insert(binding.id, sub_keys);
            }
        }
    }

    /// Resolve an expression to the product it ultimately refers to, returning
    /// (product_expr_id, fields). Follows bindings and projection chains recursively.
    fn resolve_to_product<'a>(
        expr: ExprId,
        body: BodyId,
        use_by_expr: &HashMap<ExprId, BridgeLocalBindingId>,
        products_by_expr: &HashMap<(BodyId, ExprId), &'a Vec<(FieldKey, Option<ExprId>)>>,
        projection_by_expr: &HashMap<(BodyId, ExprId), &crate::BridgeProjection>,
        binding_inits: &HashMap<BridgeLocalBindingId, ExprId>,
        visited: &mut HashSet<ExprId>,
    ) -> Option<(ExprId, &'a Vec<(FieldKey, Option<ExprId>)>)> {
        if !visited.insert(expr) {
            return None;
        }

        // Direct product?
        if let Some(product) = products_by_expr.get(&(body, expr)) {
            return Some((expr, product));
        }

        // Binding use → follow initializer
        if let Some(&binding_id) = use_by_expr.get(&expr) {
            if let Some(&init) = binding_inits.get(&binding_id) {
                if let Some(result) = resolve_to_product(
                    init,
                    body,
                    use_by_expr,
                    products_by_expr,
                    projection_by_expr,
                    binding_inits,
                    visited,
                ) {
                    return Some(result);
                }
            }
        }

        // Projection → resolve base recursively, then follow the projected field
        if let Some(proj) = projection_by_expr.get(&(body, expr)) {
            if let Some((_base_expr, base_product)) = resolve_to_product(
                proj.base,
                body,
                use_by_expr,
                products_by_expr,
                projection_by_expr,
                binding_inits,
                visited,
            ) {
                if let Some(sub_expr) = base_product
                    .iter()
                    .find_map(|(k, e)| if *k == proj.field { *e } else { None })
                {
                    return resolve_to_product(
                        sub_expr,
                        body,
                        use_by_expr,
                        products_by_expr,
                        projection_by_expr,
                        binding_inits,
                        visited,
                    );
                }
            }
        }

        None
    }

    /// Resolve an expression to the product field keys and the product's
    /// construction ExprId. Used for seeding lazy fields.
    fn resolve_product_fields_and_expr(
        expr: ExprId,
        body: BodyId,
        use_by_expr: &HashMap<ExprId, BridgeLocalBindingId>,
        all_binding_products: &HashMap<BridgeLocalBindingId, Vec<FieldKey>>,
        products_by_expr: &HashMap<(BodyId, ExprId), &Vec<(FieldKey, Option<ExprId>)>>,
        projection_by_expr: &HashMap<(BodyId, ExprId), &crate::BridgeProjection>,
        binding_inits: &HashMap<BridgeLocalBindingId, ExprId>,
        visited: &mut HashSet<ExprId>,
    ) -> Option<(ExprId, Vec<FieldKey>)> {
        // Check pre-computed binding products — resolve to the product construction expr
        if let Some(&binding_id) = use_by_expr.get(&expr) {
            if let Some(fields) = all_binding_products.get(&binding_id) {
                // Still need the product ExprId for seeding
                if let Some(&init) = binding_inits.get(&binding_id) {
                    if let Some((product_expr, _)) = resolve_to_product(
                        init,
                        body,
                        use_by_expr,
                        products_by_expr,
                        projection_by_expr,
                        binding_inits,
                        &mut visited.clone(),
                    ) {
                        return Some((product_expr, fields.clone()));
                    }
                }
                // If we can't find a product construction ExprId, we can't seed laziness
                // in a way exec_slice can consume (it keys on the product expr being lowered).
                return None;
            }
        }

        let (product_expr, product) = resolve_to_product(
            expr,
            body,
            use_by_expr,
            products_by_expr,
            projection_by_expr,
            binding_inits,
            visited,
        )?;
        Some((
            product_expr,
            product.iter().map(|(k, _)| k.clone()).collect(),
        ))
    }

    // Build binding init map for the resolver
    let binding_inits: HashMap<BridgeLocalBindingId, ExprId> = bridge
        .local_bindings
        .iter()
        .filter_map(|b| Some((b.id, b.init_expr?)))
        .collect();

    let mut seeds = bridge
        .projections
        .iter()
        .flat_map(|projection| {
            let (product_expr, fields) = resolve_product_fields_and_expr(
                projection.base,
                projection.body,
                &use_by_expr,
                &all_binding_products,
                &products_by_expr,
                &projection_by_expr,
                &binding_inits,
                &mut HashSet::new(),
            )?;
            if fields.len() <= 1 || !fields.contains(&projection.field) {
                return None;
            }

            // All siblings should be lazy, keyed by the product construction ExprId
            let siblings: Vec<_> = fields
                .iter()
                .filter(|field| **field != projection.field)
                .cloned()
                .map(|sibling| (projection.body, product_expr, sibling))
                .collect();
            Some(siblings)
        })
        .flatten()
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

/// For `let inner = outer._0` where `outer` is `((safe(), bomb()), ...)`,
/// derive that `inner` is effectively a product with the fields of the
/// sub-expression at `outer._0`.
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
            crate::BridgeCallSiteKind::Other => {
                if site.target.is_some() {
                    seeds.push((site.body, site.expr, Observation::NeedCallable));
                }
            }
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
        for (next_id, (_, _, obs)) in seeds
            .iter()
            .filter(|(seed_body, _, _)| *seed_body == body.id)
            .enumerate()
        {
            body.use_observations
                .insert(UseSiteId(next_id as _), obs.clone());
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
                | crate::BridgeCallSiteKind::LocalCallableBinding
                | crate::BridgeCallSiteKind::Other => Some((site.body, site.expr, target)),
                crate::BridgeCallSiteKind::StdlibBoundary(_) => None,
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
            .map(|(name, _, source)| CaptureSlot {
                name,
                source,
                incoming_use_observation: Observation::NoObs,
            })
            .collect();

        layouts.push(ClosureEnvLayout {
            id: LayoutId(index as u32),
            source: SourceExpr {
                body: body_id,
                expr: closure_expr.id,
            },
            incoming_use_observation: Observation::NoObs,
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
        incoming_use_observation: Observation::NoObs,
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

fn kernel_shell(
    body: &BridgeBody,
    bridge: &SingleSourceBridge,
    extracted_exprs: Option<&BTreeSet<ExprId>>,
) -> KernelRegion {
    let body_exprs = bridge
        .exprs
        .iter()
        .filter(|expr| expr.body == Some(body.id))
        .filter(|expr| {
            extracted_exprs
                .map(|set| !set.contains(&expr.id))
                .unwrap_or(true)
        })
        .map(|expr| expr.id)
        .collect::<BTreeSet<_>>();

    // Build typed kernel params from the body's parameters and local bindings.
    let params = body
        .params
        .iter()
        .filter_map(|param| {
            let binding = bridge
                .local_bindings
                .iter()
                .find(|b| b.body == body.id && b.name_span == param.span)?;
            let ty = bridge.expr_types.values().next().and_then(|_| {
                // Look up the type from binding_types or expr_types by finding
                // an expression at this param's span
                bridge
                    .exprs
                    .iter()
                    .find(|expr| expr.body == Some(body.id) && expr.span == param.span)
                    .and_then(|expr| bridge.expr_types.get(&expr.id).cloned())
            });
            Some(crate::KernelParam {
                name: param.name.clone(),
                source: None,
                ty,
                binding: Some(binding.id),
            })
        })
        .collect();

    // Result type: find the last expression in the body and look up its type.
    let result_ty = body_exprs
        .iter()
        .max()
        .and_then(|expr_id| bridge.expr_types.get(expr_id).cloned());

    KernelRegion {
        id: KernelId(body.id.0),
        body: body.id,
        exprs: body_exprs,
        params,
        result_obs: Observation::NeedValue,
        result_ty,
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
