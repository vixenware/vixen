use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
};

use vx_parser::{AstNode, Expr, ImportExpr, Parsed, SourceFile, SyntaxError};
use vx_resolve::{
    DefId, DefKind, Definition, ImportGraph, NameResolution, ResolveError, SourceFileRef, SourceId,
    SourceSpan, TypeCheckResult, check_capabilities, collect_types_for_sources, resolve_in_program,
    stdlib_sources, type_check,
};
use vx_types::Ty;

use crate::{
    BodyId, ExprId, FieldKey, RuntimeBoundaryKind, SourceExpr, boundary::classify_runtime_boundary,
};

#[derive(Debug, Clone)]
pub struct BridgeParam {
    pub name: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct BridgeBody {
    pub id: BodyId,
    pub name: String,
    pub span: SourceSpan,
    pub def_id: Option<DefId>,
    pub params: Vec<BridgeParam>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeExprKind {
    Call,
    Closure,
    Field,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeExpr {
    pub id: ExprId,
    pub span: SourceSpan,
    pub body: Option<BodyId>,
    pub kind: BridgeExprKind,
}

#[derive(Debug, Clone)]
pub struct BridgeResolvedRef {
    pub span: SourceSpan,
    pub name: String,
    pub target: Result<Definition, ResolveError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeProduct {
    pub expr: ExprId,
    pub body: BodyId,
    pub fields: Vec<FieldKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeProjection {
    pub expr: ExprId,
    pub body: BodyId,
    pub base: ExprId,
    pub base_binding: Option<String>,
    pub field: FieldKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeBindingProduct {
    pub body: BodyId,
    pub binding: String,
    pub fields: Vec<FieldKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeLocalBindingId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeLocalBindingKind {
    Let,
    Parameter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeLocalBinding {
    pub id: BridgeLocalBindingId,
    pub body: BodyId,
    pub name: String,
    pub name_span: SourceSpan,
    pub kind: BridgeLocalBindingKind,
    pub def_id: DefId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeLocalUse {
    pub body: BodyId,
    pub expr: ExprId,
    pub binding: BridgeLocalBindingId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeLocalClosureInit {
    pub binding: BridgeLocalBindingId,
    pub source: SourceExpr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeCallSiteKind {
    DirectNamedFunction,
    LocalCallableBinding,
    StdlibBoundary(RuntimeBoundaryKind),
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeCallSite {
    pub body: BodyId,
    pub expr: ExprId,
    pub kind: BridgeCallSiteKind,
    pub target: Option<DefId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BridgeBoundaryArgBinding {
    pub body: BodyId,
    pub call_expr: ExprId,
    pub binding: BridgeLocalBindingId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeBoundaryArgClosure {
    pub body: BodyId,
    pub call_expr: ExprId,
    pub closure_source: SourceExpr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BridgeLocalBoundaryInit {
    pub body: BodyId,
    pub binding: BridgeLocalBindingId,
    pub call_expr: ExprId,
}

#[derive(Debug, Clone)]
pub struct SingleSourceBridge {
    pub source_id: SourceId,
    pub parsed: Parsed<SourceFile>,
    pub source_files: BTreeMap<u32, SourceFile>,
    pub source_paths: BTreeMap<u32, String>,
    pub bodies: Vec<BridgeBody>,
    pub exprs: Vec<BridgeExpr>,
    pub resolved_refs: Vec<BridgeResolvedRef>,
    pub products: Vec<BridgeProduct>,
    pub projections: Vec<BridgeProjection>,
    pub binding_products: Vec<BridgeBindingProduct>,
    pub local_bindings: Vec<BridgeLocalBinding>,
    pub local_uses: Vec<BridgeLocalUse>,
    pub local_closure_inits: Vec<BridgeLocalClosureInit>,
    pub call_sites: Vec<BridgeCallSite>,
    pub boundary_arg_bindings: Vec<BridgeBoundaryArgBinding>,
    pub boundary_arg_closures: Vec<BridgeBoundaryArgClosure>,
    pub local_boundary_inits: Vec<BridgeLocalBoundaryInit>,
    pub expr_types: BTreeMap<ExprId, Ty>,
    pub parse_errors: Vec<SyntaxError>,
    pub resolve_errors: Vec<ResolveError>,
    pub type_errors: Vec<vx_resolve::TypeError>,
    pub cap_errors: Vec<vx_resolve::CapError>,
}

impl SingleSourceBridge {
    pub fn from_source_text(text: &str) -> Self {
        let source_id = SourceId(0);
        let parsed = vx_parser::parse_source_file(text);
        let parse_errors = parsed.errors().to_vec();
        let file = parsed.tree();

        let (sources, imports, source_paths) = build_program_sources_from_single_file(file.clone());
        let source_files = sources
            .iter()
            .map(|source| (source.id.0, source.file.clone()))
            .collect::<BTreeMap<_, _>>();

        let resolution = resolve_in_program(
            SourceFileRef {
                id: source_id,
                file: file.clone(),
            },
            &sources,
            imports,
        );
        let types = collect_types_for_sources(&sources);
        let type_check = type_check(&file, &resolution, &types);
        let cap_check = check_capabilities(&file, &resolution, &types, &type_check);

        let resolved_refs = collect_resolved_refs(&resolution);
        let collected = collect_project_bridge_data(
            &sources,
            &source_paths,
            &resolution,
            &type_check,
            &resolved_refs,
        );

        Self {
            source_id,
            parsed,
            source_files,
            source_paths,
            bodies: collected.bodies,
            exprs: collected.exprs,
            resolved_refs,
            products: collected.products,
            projections: collected.projections,
            binding_products: collected.binding_products,
            local_bindings: collected.local_bindings,
            local_uses: collected.local_uses,
            local_closure_inits: collected.local_closure_inits,
            call_sites: collected.call_sites,
            boundary_arg_bindings: collected.boundary_arg_bindings,
            boundary_arg_closures: collected.boundary_arg_closures,
            local_boundary_inits: collected.local_boundary_inits,
            expr_types: collected.expr_types,
            parse_errors,
            resolve_errors: resolution.errors,
            type_errors: type_check.errors,
            cap_errors: cap_check.errors,
        }
    }

    pub fn from_entry_file(entry_path: &Path) -> io::Result<Self> {
        let source_id = SourceId(0);
        let entry_text = fs::read_to_string(entry_path)?;
        let parsed = vx_parser::parse_source_file(&entry_text);
        let parse_errors = parsed.errors().to_vec();
        let file = parsed.tree();

        let (sources, imports, source_paths) = build_program_sources_from_entry_file(entry_path)?;
        let source_files = sources
            .iter()
            .map(|source| (source.id.0, source.file.clone()))
            .collect::<BTreeMap<_, _>>();

        let resolution = resolve_in_program(
            SourceFileRef {
                id: source_id,
                file: file.clone(),
            },
            &sources,
            imports,
        );
        let types = collect_types_for_sources(&sources);
        let type_check = type_check(&file, &resolution, &types);
        let cap_check = check_capabilities(&file, &resolution, &types, &type_check);

        let resolved_refs = collect_resolved_refs(&resolution);
        let collected = collect_project_bridge_data(
            &sources,
            &source_paths,
            &resolution,
            &type_check,
            &resolved_refs,
        );

        Ok(Self {
            source_id,
            parsed,
            source_files,
            source_paths,
            bodies: collected.bodies,
            exprs: collected.exprs,
            resolved_refs,
            products: collected.products,
            projections: collected.projections,
            binding_products: collected.binding_products,
            local_bindings: collected.local_bindings,
            local_uses: collected.local_uses,
            local_closure_inits: collected.local_closure_inits,
            call_sites: collected.call_sites,
            boundary_arg_bindings: collected.boundary_arg_bindings,
            boundary_arg_closures: collected.boundary_arg_closures,
            local_boundary_inits: collected.local_boundary_inits,
            expr_types: collected.expr_types,
            parse_errors,
            resolve_errors: resolution.errors,
            type_errors: type_check.errors,
            cap_errors: cap_check.errors,
        })
    }
}

fn build_program_sources_from_single_file(
    file: SourceFile,
) -> (Vec<SourceFileRef>, ImportGraph, BTreeMap<u32, String>) {
    let source_id = SourceId(0);
    let mut sources = vec![SourceFileRef {
        id: source_id,
        file: file.clone(),
    }];
    let mut imports = ImportGraph::new();
    let mut source_paths = BTreeMap::new();
    source_paths.insert(source_id.0, "<inline-source>".to_owned());
    let mut next_source_id = 1u32;

    for stdlib in stdlib_sources() {
        let parsed_stdlib = vx_parser::parse_source_file(stdlib.text);
        let stdlib_id = SourceId(next_source_id);
        next_source_id += 1;
        imports.add_import(source_id, stdlib_id);
        source_paths.insert(stdlib_id.0, stdlib.logical_path.to_owned());
        sources.push(SourceFileRef {
            id: stdlib_id,
            file: parsed_stdlib.tree(),
        });
    }

    (sources, imports, source_paths)
}

fn build_program_sources_from_entry_file(
    entry_path: &Path,
) -> io::Result<(Vec<SourceFileRef>, ImportGraph, BTreeMap<u32, String>)> {
    let entry_abs = entry_path.canonicalize()?;
    let mut imports = ImportGraph::new();
    let mut source_paths = BTreeMap::new();
    let mut path_to_source = BTreeMap::<PathBuf, SourceId>::new();
    let mut parsed_by_source = HashMap::<SourceId, Parsed<SourceFile>>::new();
    let mut queue = VecDeque::<PathBuf>::new();
    let mut next_source_id = 0u32;

    queue.push_back(entry_abs.clone());

    while let Some(path) = queue.pop_front() {
        let source_id = if let Some(id) = path_to_source.get(&path) {
            *id
        } else {
            let id = SourceId(next_source_id);
            next_source_id += 1;
            path_to_source.insert(path.clone(), id);
            source_paths.insert(id.0, path.display().to_string());
            id
        };

        if parsed_by_source.contains_key(&source_id) {
            continue;
        }

        let text = fs::read_to_string(&path)?;
        let parsed = vx_parser::parse_source_file(&text);
        let file = parsed.tree();

        let base_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut import_paths = file
            .syntax()
            .descendants()
            .filter_map(ImportExpr::cast)
            .filter_map(|import| import.path())
            .filter_map(|raw| resolve_local_import_path(&base_dir, &raw))
            .collect::<Vec<_>>();
        import_paths.sort();
        import_paths.dedup();

        for imported_path in import_paths {
            let imported_id = if let Some(existing) = path_to_source.get(&imported_path) {
                *existing
            } else {
                let id = SourceId(next_source_id);
                next_source_id += 1;
                path_to_source.insert(imported_path.clone(), id);
                source_paths.insert(id.0, imported_path.display().to_string());
                queue.push_back(imported_path.clone());
                id
            };
            imports.add_import(source_id, imported_id);
        }

        parsed_by_source.insert(source_id, parsed);
    }

    let mut sources = parsed_by_source
        .iter()
        .map(|(id, parsed)| SourceFileRef {
            id: *id,
            file: parsed.tree(),
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|source| source.id.0);

    let user_source_ids = sources.iter().map(|source| source.id).collect::<Vec<_>>();
    for stdlib in stdlib_sources() {
        let parsed_stdlib = vx_parser::parse_source_file(stdlib.text);
        let stdlib_id = SourceId(next_source_id);
        next_source_id += 1;
        source_paths.insert(stdlib_id.0, stdlib.logical_path.to_owned());
        for user_id in &user_source_ids {
            imports.add_import(*user_id, stdlib_id);
        }
        sources.push(SourceFileRef {
            id: stdlib_id,
            file: parsed_stdlib.tree(),
        });
    }

    sources.sort_by_key(|source| source.id.0);
    Ok((sources, imports, source_paths))
}

fn resolve_local_import_path(base_dir: &Path, raw: &str) -> Option<PathBuf> {
    if raw.contains(':') {
        return None;
    }
    let joined = if raw.starts_with('/') {
        PathBuf::from(raw)
    } else {
        base_dir.join(raw)
    };

    let candidate = if joined.extension().is_some() {
        joined
    } else {
        joined.with_extension("vx")
    };
    candidate.canonicalize().ok()
}

#[derive(Default)]
struct CollectedBridgeData {
    bodies: Vec<BridgeBody>,
    exprs: Vec<BridgeExpr>,
    products: Vec<BridgeProduct>,
    projections: Vec<BridgeProjection>,
    binding_products: Vec<BridgeBindingProduct>,
    local_bindings: Vec<BridgeLocalBinding>,
    local_uses: Vec<BridgeLocalUse>,
    local_closure_inits: Vec<BridgeLocalClosureInit>,
    call_sites: Vec<BridgeCallSite>,
    boundary_arg_bindings: Vec<BridgeBoundaryArgBinding>,
    boundary_arg_closures: Vec<BridgeBoundaryArgClosure>,
    local_boundary_inits: Vec<BridgeLocalBoundaryInit>,
    expr_types: BTreeMap<ExprId, Ty>,
}

fn is_stdlib_source_path(path: &str) -> bool {
    stdlib_sources()
        .iter()
        .any(|stdlib| stdlib.logical_path == path)
}

fn collect_project_bridge_data(
    sources: &[SourceFileRef],
    source_paths: &BTreeMap<u32, String>,
    resolution: &NameResolution,
    type_check: &TypeCheckResult,
    resolved_refs: &[BridgeResolvedRef],
) -> CollectedBridgeData {
    let mut collected = CollectedBridgeData::default();
    let mut user_sources = sources
        .iter()
        .filter(|source| {
            source_paths
                .get(&source.id.0)
                .map(|path| !is_stdlib_source_path(path))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    user_sources.sort_by_key(|source| source.id.0);

    for source in user_sources {
        let source_id = source.id;
        let file = source.file.clone();

        let local_bodies = collect_bodies(source_id, &file, resolution);
        let local_exprs = collect_exprs(source_id, &file, &local_bodies);
        let local_products = collect_products(source_id, &file, &local_exprs);
        let local_projections = collect_projections(source_id, &file, &local_exprs);
        let local_binding_products = collect_binding_products(source_id, &file, &local_bodies);
        let local_bindings = collect_local_bindings(source_id, resolution, &local_bodies);
        let local_uses = collect_local_uses(resolution, &local_exprs, &local_bindings);
        let local_closure_inits = collect_local_closure_inits(
            source_id,
            &file,
            &local_exprs,
            &local_bindings,
            &local_bodies,
        );
        let imported_build_targets =
            imported_build_targets_for_source(source_id, &file, source_paths, resolution);
        let local_call_sites = collect_call_sites(
            source_id,
            &file,
            &local_exprs,
            resolved_refs,
            &imported_build_targets,
        );
        let (local_boundary_arg_bindings, local_boundary_arg_closures, local_boundary_inits) =
            collect_boundary_call_links(
                source_id,
                &file,
                &local_exprs,
                resolved_refs,
                &local_bindings,
                &local_closure_inits,
                &local_call_sites,
            );
        let local_expr_types = collect_expr_types(&local_exprs, type_check);

        let body_offset = collected.bodies.len() as u32;
        let expr_offset = collected.exprs.len() as u32;
        let binding_offset = collected.local_bindings.len() as u32;

        let body_map = local_bodies
            .iter()
            .map(|body| (body.id, BodyId(body.id.0 + body_offset)))
            .collect::<HashMap<_, _>>();
        let expr_map = local_exprs
            .iter()
            .map(|expr| (expr.id, ExprId(expr.id.0 + expr_offset)))
            .collect::<HashMap<_, _>>();
        let binding_map = local_bindings
            .iter()
            .map(|binding| {
                (
                    binding.id,
                    BridgeLocalBindingId(binding.id.0 + binding_offset),
                )
            })
            .collect::<HashMap<_, _>>();

        collected
            .bodies
            .extend(local_bodies.into_iter().map(|mut body| {
                body.id = body_map[&body.id];
                body
            }));
        collected
            .exprs
            .extend(local_exprs.into_iter().map(|mut expr| {
                expr.id = expr_map[&expr.id];
                expr.body = expr.body.and_then(|body| body_map.get(&body).copied());
                expr
            }));
        collected
            .products
            .extend(local_products.into_iter().map(|mut product| {
                product.expr = expr_map[&product.expr];
                product.body = body_map[&product.body];
                product
            }));
        collected
            .projections
            .extend(local_projections.into_iter().map(|mut projection| {
                projection.expr = expr_map[&projection.expr];
                projection.base = expr_map[&projection.base];
                projection.body = body_map[&projection.body];
                projection
            }));
        collected
            .binding_products
            .extend(local_binding_products.into_iter().map(|mut product| {
                product.body = body_map[&product.body];
                product
            }));
        collected
            .local_bindings
            .extend(local_bindings.into_iter().map(|mut binding| {
                binding.id = binding_map[&binding.id];
                binding.body = body_map[&binding.body];
                binding
            }));
        collected
            .local_uses
            .extend(local_uses.into_iter().map(|mut usage| {
                usage.body = body_map[&usage.body];
                usage.expr = expr_map[&usage.expr];
                usage.binding = binding_map[&usage.binding];
                usage
            }));
        collected
            .local_closure_inits
            .extend(local_closure_inits.into_iter().map(|mut init| {
                init.binding = binding_map[&init.binding];
                init.source.body = body_map[&init.source.body];
                init.source.expr = expr_map[&init.source.expr];
                init
            }));
        collected
            .call_sites
            .extend(local_call_sites.into_iter().map(|mut site| {
                site.body = body_map[&site.body];
                site.expr = expr_map[&site.expr];
                site
            }));
        collected
            .boundary_arg_bindings
            .extend(local_boundary_arg_bindings.into_iter().map(|mut arg| {
                arg.body = body_map[&arg.body];
                arg.call_expr = expr_map[&arg.call_expr];
                arg.binding = binding_map[&arg.binding];
                arg
            }));
        collected
            .boundary_arg_closures
            .extend(local_boundary_arg_closures.into_iter().map(|mut arg| {
                arg.body = body_map[&arg.body];
                arg.call_expr = expr_map[&arg.call_expr];
                arg.closure_source.body = body_map[&arg.closure_source.body];
                arg.closure_source.expr = expr_map[&arg.closure_source.expr];
                arg
            }));
        collected
            .local_boundary_inits
            .extend(local_boundary_inits.into_iter().map(|mut init| {
                init.body = body_map[&init.body];
                init.binding = binding_map[&init.binding];
                init.call_expr = expr_map[&init.call_expr];
                init
            }));
        collected.expr_types.extend(
            local_expr_types
                .into_iter()
                .map(|(expr_id, ty)| (expr_map[&expr_id], ty)),
        );
    }

    collected
}

fn parse_field_key(token: &str) -> FieldKey {
    if let Some(index) = token
        .strip_prefix('_')
        .and_then(|digits| digits.parse::<u32>().ok())
    {
        FieldKey::Index(index)
    } else {
        FieldKey::Name(token.to_owned())
    }
}

fn product_fields_for_expr(expr: &Expr) -> Option<Vec<FieldKey>> {
    match expr {
        Expr::Tuple(tuple) => Some(
            tuple
                .items()
                .enumerate()
                .map(|(index, _)| FieldKey::Index(index as u32))
                .collect(),
        ),
        Expr::Record(record) => Some(
            record
                .fields()
                .filter_map(|field| field.name())
                .map(|name| FieldKey::Name(name.text().to_owned()))
                .collect(),
        ),
        Expr::StructLiteral(struct_lit) => Some(
            struct_lit
                .fields()
                .filter_map(|field| field.name())
                .map(|name| FieldKey::Name(name.text().to_owned()))
                .collect(),
        ),
        _ => None,
    }
}

fn collect_bodies(
    source_id: SourceId,
    file: &SourceFile,
    resolution: &NameResolution,
) -> Vec<BridgeBody> {
    let mut bodies = Vec::new();

    for fn_decl in file.items().filter_map(|item| match item {
        vx_parser::Item::FnDecl(decl) => Some(decl),
        _ => None,
    }) {
        if let Some(name) = fn_decl.name() {
            let body_id = BodyId(bodies.len() as u32);
            let params = fn_decl
                .parameters()
                .filter_map(|parameter| {
                    let name = parameter.name()?;
                    Some(BridgeParam {
                        name: name.text().to_owned(),
                        span: SourceSpan::new(source_id, name.text_range()),
                    })
                })
                .collect();
            let name_span = SourceSpan::new(source_id, name.text_range());
            let def_id = resolution
                .scopes
                .all_definitions_in(source_id)
                .into_iter()
                .find(|def| def.kind == DefKind::Function && def.name_span == name_span)
                .map(|def| def.id);
            bodies.push(BridgeBody {
                id: body_id,
                name: name.text().to_owned(),
                span: SourceSpan::new(source_id, fn_decl.syntax().text_range()),
                def_id,
                params,
            });
        }
    }

    bodies
}

fn collect_exprs(source_id: SourceId, file: &SourceFile, bodies: &[BridgeBody]) -> Vec<BridgeExpr> {
    file.syntax()
        .descendants()
        .filter_map(Expr::cast)
        .enumerate()
        .map(|(index, expr)| {
            let span = SourceSpan::new(source_id, expr.syntax().text_range());
            let kind = match expr {
                Expr::Call(_) => BridgeExprKind::Call,
                Expr::Closure(_) => BridgeExprKind::Closure,
                Expr::Field(_) => BridgeExprKind::Field,
                _ => BridgeExprKind::Other,
            };
            BridgeExpr {
                id: ExprId(index as u32),
                span,
                body: body_for_range(span.range, bodies),
                kind,
            }
        })
        .collect::<Vec<_>>()
}

fn body_for_range(range: vx_parser::TextRange, bodies: &[BridgeBody]) -> Option<BodyId> {
    let mut best: Option<(BodyId, u32)> = None;
    for body in bodies {
        if body.span.range.contains_range(range) {
            let len = u32::from(body.span.range.end()) - u32::from(body.span.range.start());
            match best {
                Some((_, best_len)) if len >= best_len => {}
                _ => best = Some((body.id, len)),
            }
        }
    }
    best.map(|(id, _)| id)
}

fn collect_resolved_refs(resolution: &NameResolution) -> Vec<BridgeResolvedRef> {
    resolution
        .refs
        .iter()
        .map(|resolved| BridgeResolvedRef {
            span: resolved.span,
            name: resolved.name.clone(),
            target: resolved.target.clone(),
        })
        .collect()
}

fn expr_id_by_span(exprs: &[BridgeExpr]) -> HashMap<SourceSpan, ExprId> {
    exprs.iter().map(|expr| (expr.span, expr.id)).collect()
}

fn collect_products(
    source_id: SourceId,
    file: &SourceFile,
    exprs: &[BridgeExpr],
) -> Vec<BridgeProduct> {
    let by_span = expr_id_by_span(exprs);
    let by_id = exprs
        .iter()
        .map(|expr| (expr.id, expr))
        .collect::<HashMap<_, _>>();

    let mut products = file
        .syntax()
        .descendants()
        .filter_map(Expr::cast)
        .filter_map(|expr| {
            let fields = product_fields_for_expr(&expr)?;
            let span = SourceSpan::new(source_id, expr.syntax().text_range());

            let expr_id = by_span.get(&span).copied()?;
            let body = by_id.get(&expr_id)?.body?;
            Some(BridgeProduct {
                expr: expr_id,
                body,
                fields,
            })
        })
        .collect::<Vec<_>>();

    products.sort_by_key(|product| (product.body.0, product.expr.0));
    products
}

fn collect_projections(
    source_id: SourceId,
    file: &SourceFile,
    exprs: &[BridgeExpr],
) -> Vec<BridgeProjection> {
    let by_span = expr_id_by_span(exprs);
    let by_id = exprs
        .iter()
        .map(|expr| (expr.id, expr))
        .collect::<HashMap<_, _>>();

    let mut projections = file
        .syntax()
        .descendants()
        .filter_map(vx_parser::FieldExpr::cast)
        .filter_map(|field_expr| {
            let receiver = field_expr.receiver()?;
            let field_name = field_expr.field_name()?;
            let projection_span = SourceSpan::new(source_id, field_expr.syntax().text_range());
            let receiver_span = SourceSpan::new(source_id, receiver.syntax().text_range());
            let expr = by_span.get(&projection_span).copied()?;
            let base = by_span.get(&receiver_span).copied()?;
            let body = by_id.get(&expr)?.body?;

            Some(BridgeProjection {
                expr,
                body,
                base,
                base_binding: match receiver {
                    Expr::NameRef(name_ref) => {
                        name_ref.token().map(|token| token.text().to_owned())
                    }
                    _ => None,
                },
                field: parse_field_key(field_name.text()),
            })
        })
        .collect::<Vec<_>>();

    projections.sort_by_key(|projection| (projection.body.0, projection.expr.0));
    projections
}

fn collect_binding_products(
    source_id: SourceId,
    file: &SourceFile,
    bodies: &[BridgeBody],
) -> Vec<BridgeBindingProduct> {
    let mut products = file
        .syntax()
        .descendants()
        .filter_map(vx_parser::LetStmt::cast)
        .filter_map(|let_stmt| {
            let binding = let_stmt.name()?.text().to_owned();
            let expr = let_stmt.expr()?;
            let fields = product_fields_for_expr(&expr)?;
            let span = SourceSpan::new(source_id, let_stmt.syntax().text_range());
            let body = body_for_range(span.range, bodies)?;
            Some(BridgeBindingProduct {
                body,
                binding,
                fields,
            })
        })
        .collect::<Vec<_>>();

    products.sort_by_key(|product| (product.body.0, product.binding.clone()));
    products
}

fn collect_expr_types(exprs: &[BridgeExpr], checked: &TypeCheckResult) -> BTreeMap<ExprId, Ty> {
    let by_span = exprs
        .iter()
        .map(|expr| (expr.span, expr.id))
        .collect::<HashMap<_, _>>();

    checked
        .expr_types
        .iter()
        .filter_map(|(span, ty)| by_span.get(span).copied().map(|id| (id, ty.clone())))
        .collect()
}

fn enclosing_expr_source(span: SourceSpan, exprs: &[BridgeExpr]) -> Option<(BodyId, ExprId)> {
    exprs
        .iter()
        .filter_map(|expr| {
            let body = expr.body?;
            if expr.span.source == span.source && expr.span.range.contains_range(span.range) {
                let len = u32::from(expr.span.range.end()) - u32::from(expr.span.range.start());
                Some((len, body, expr.id))
            } else {
                None
            }
        })
        .min_by_key(|(len, _, expr)| (*len, expr.0))
        .map(|(_, body, expr)| (body, expr))
}

fn collect_local_bindings(
    source_id: SourceId,
    resolution: &NameResolution,
    bodies: &[BridgeBody],
) -> Vec<BridgeLocalBinding> {
    let mut entries = resolution
        .scopes
        .all_definitions_in(source_id)
        .into_iter()
        .filter_map(|def| {
            let body = body_for_range(def.name_span.range, bodies)?;
            let kind = match def.kind {
                DefKind::Binding => BridgeLocalBindingKind::Let,
                DefKind::Parameter => BridgeLocalBindingKind::Parameter,
                _ => return None,
            };

            Some((body, def.name_span, def.name.clone(), kind, def.id))
        })
        .collect::<Vec<_>>();

    entries.sort_by_key(|(body, span, name, kind, def_id)| {
        let kind_rank = match kind {
            BridgeLocalBindingKind::Parameter => 0u8,
            BridgeLocalBindingKind::Let => 1,
        };
        let (def_source, def_local) = match def_id {
            DefId::Source { source, local } => (source.0, *local),
        };
        (
            body.0,
            u32::from(span.range.start()),
            u32::from(span.range.end()),
            kind_rank,
            name.clone(),
            def_source,
            def_local,
        )
    });
    entries
        .dedup_by(|lhs, rhs| lhs.0 == rhs.0 && lhs.1 == rhs.1 && lhs.2 == rhs.2 && lhs.3 == rhs.3);

    entries
        .into_iter()
        .enumerate()
        .map(
            |(index, (body, name_span, name, kind, def_id))| BridgeLocalBinding {
                id: BridgeLocalBindingId(index as u32),
                body,
                name,
                name_span,
                kind,
                def_id,
            },
        )
        .collect()
}

fn collect_local_uses(
    resolution: &NameResolution,
    exprs: &[BridgeExpr],
    local_bindings: &[BridgeLocalBinding],
) -> Vec<BridgeLocalUse> {
    let binding_by_def = local_bindings
        .iter()
        .map(|binding| (binding.def_id, binding.id))
        .collect::<HashMap<_, _>>();

    let mut uses = resolution
        .refs
        .iter()
        .filter_map(|resolved| {
            let target = resolved.target.as_ref().ok()?;
            let binding = binding_by_def.get(&target.id).copied()?;
            let (body, expr) = enclosing_expr_source(resolved.span, exprs)?;
            Some(BridgeLocalUse {
                body,
                expr,
                binding,
            })
        })
        .collect::<Vec<_>>();
    uses.sort_by_key(|usage| (usage.body.0, usage.expr.0, usage.binding.0));
    uses.dedup();
    uses
}

fn collect_local_closure_inits(
    source_id: SourceId,
    file: &SourceFile,
    exprs: &[BridgeExpr],
    local_bindings: &[BridgeLocalBinding],
    bodies: &[BridgeBody],
) -> Vec<BridgeLocalClosureInit> {
    let by_span = expr_id_by_span(exprs);

    let mut inits = file
        .syntax()
        .descendants()
        .filter_map(vx_parser::LetStmt::cast)
        .filter_map(|let_stmt| {
            let name = let_stmt.name()?;
            let body = body_for_range(let_stmt.syntax().text_range(), bodies)?;
            let binding = local_bindings
                .iter()
                .find(|binding| {
                    binding.kind == BridgeLocalBindingKind::Let
                        && binding.body == body
                        && binding.name == name.text()
                })
                .map(|binding| binding.id)?;

            let init_expr = let_stmt.expr()?;
            if !matches!(init_expr, Expr::Closure(_)) {
                return None;
            }
            let init_span = SourceSpan::new(source_id, init_expr.syntax().text_range());
            let closure_expr = by_span.get(&init_span).copied()?;

            Some(BridgeLocalClosureInit {
                binding,
                source: SourceExpr {
                    body,
                    expr: closure_expr,
                },
            })
        })
        .collect::<Vec<_>>();

    inits.sort_by_key(|init| (init.source.body.0, init.binding.0, init.source.expr.0));
    inits.dedup();
    inits
}

fn collect_call_sites(
    source_id: SourceId,
    file: &SourceFile,
    exprs: &[BridgeExpr],
    resolved_refs: &[BridgeResolvedRef],
    imported_build_targets: &HashMap<String, DefId>,
) -> Vec<BridgeCallSite> {
    let by_span = expr_id_by_span(exprs);
    let by_id = exprs
        .iter()
        .map(|expr| (expr.id, expr))
        .collect::<HashMap<_, _>>();

    let mut call_sites = file
        .syntax()
        .descendants()
        .filter_map(vx_parser::CallExpr::cast)
        .filter_map(|call| {
            let call_span = SourceSpan::new(source_id, call.syntax().text_range());
            let call_expr = by_span.get(&call_span).copied()?;
            let body = by_id.get(&call_expr)?.body?;
            let callee = call.callee()?;
            let callee_is_name_ref = matches!(callee, Expr::NameRef(_));
            let callee_span = SourceSpan::new(source_id, callee.syntax().text_range());

            let resolved = resolved_refs
                .iter()
                .filter(|resolved| {
                    resolved.span.source == callee_span.source
                        && callee_span.range.contains_range(resolved.span.range)
                })
                .min_by_key(|resolved| {
                    let classified_kind = if let Some(boundary_kind) =
                        classify_runtime_boundary(source_id, resolved)
                    {
                        BridgeCallSiteKind::StdlibBoundary(boundary_kind)
                    } else {
                        match resolved.target.as_ref().ok().map(|target| target.kind) {
                            Some(DefKind::Function) => BridgeCallSiteKind::DirectNamedFunction,
                            Some(DefKind::Binding) | Some(DefKind::Parameter)
                                if callee_is_name_ref =>
                            {
                                BridgeCallSiteKind::LocalCallableBinding
                            }
                            _ => BridgeCallSiteKind::Other,
                        }
                    };
                    let kind_rank = match classified_kind {
                        BridgeCallSiteKind::DirectNamedFunction => 0u8,
                        BridgeCallSiteKind::LocalCallableBinding => 1,
                        BridgeCallSiteKind::StdlibBoundary(_) => 2,
                        BridgeCallSiteKind::Other => 10,
                    };
                    let len = u32::from(resolved.span.range.end())
                        - u32::from(resolved.span.range.start());
                    (kind_rank, len, u32::from(resolved.span.range.start()))
                });

            let kind = if let Some(resolved) = resolved {
                if let Some(boundary_kind) = classify_runtime_boundary(source_id, resolved) {
                    BridgeCallSiteKind::StdlibBoundary(boundary_kind)
                } else {
                    match resolved.target.as_ref().ok().map(|target| target.kind) {
                        Some(DefKind::Function) => BridgeCallSiteKind::DirectNamedFunction,
                        Some(DefKind::Binding) | Some(DefKind::Parameter) if callee_is_name_ref => {
                            BridgeCallSiteKind::LocalCallableBinding
                        }
                        _ => BridgeCallSiteKind::Other,
                    }
                }
            } else {
                BridgeCallSiteKind::Other
            };
            let mut target = resolved.and_then(|resolved| {
                let target = resolved.target.as_ref().ok()?;
                if !callee_is_name_ref && matches!(target.kind, DefKind::Parameter) {
                    return None;
                }
                Some(target.id)
            });
            let mut kind = kind;
            if matches!(
                kind,
                BridgeCallSiteKind::LocalCallableBinding | BridgeCallSiteKind::Other
            ) && let Expr::Field(field) = callee
                && let Some(receiver) = field.receiver()
                && let Expr::NameRef(name_ref) = receiver
                && let Some(name_token) = name_ref.token()
                && let Some(field_name) = field.field_name()
                && field_name.text() == "build"
                && let Some(def_id) = imported_build_targets.get(name_token.text())
            {
                kind = BridgeCallSiteKind::DirectNamedFunction;
                target = Some(*def_id);
            }

            Some(BridgeCallSite {
                body,
                expr: call_expr,
                kind,
                target,
            })
        })
        .collect::<Vec<_>>();

    call_sites.sort_by_key(|site| {
        let rank = match site.kind {
            BridgeCallSiteKind::DirectNamedFunction => 0u8,
            BridgeCallSiteKind::LocalCallableBinding => 1,
            BridgeCallSiteKind::StdlibBoundary(kind) => 2 + kind as u8,
            BridgeCallSiteKind::Other => 10,
        };
        (site.body.0, site.expr.0, rank)
    });
    call_sites.dedup();
    call_sites
}

fn imported_build_targets_for_source(
    source_id: SourceId,
    file: &SourceFile,
    source_paths: &BTreeMap<u32, String>,
    resolution: &NameResolution,
) -> HashMap<String, DefId> {
    let Some(source_path) = source_paths.get(&source_id.0) else {
        return HashMap::new();
    };
    let base_dir = Path::new(source_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let path_to_source = source_paths
        .iter()
        .map(|(id, path)| (path.clone(), SourceId(*id)))
        .collect::<HashMap<_, _>>();
    let mut out = HashMap::new();

    for let_stmt in file
        .syntax()
        .descendants()
        .filter_map(vx_parser::LetStmt::cast)
    {
        let Some(alias) = let_stmt.name().map(|name| name.text().to_owned()) else {
            continue;
        };
        let Some(expr) = let_stmt.expr() else {
            continue;
        };
        let Some(import_expr) = ImportExpr::cast(expr.syntax().clone()) else {
            continue;
        };
        let Some(raw_path) = import_expr.path() else {
            continue;
        };
        let Some(resolved_path) = resolve_local_import_path(&base_dir, &raw_path) else {
            continue;
        };
        let resolved_text = resolved_path.display().to_string();
        let Some(imported_source_id) = path_to_source.get(&resolved_text).copied() else {
            continue;
        };
        let build_def = resolution
            .scopes
            .all_definitions_in(imported_source_id)
            .into_iter()
            .filter(|def| def.kind == DefKind::Function && def.name == "build")
            .min_by_key(|def| match def.id {
                DefId::Source { local, .. } => local,
            })
            .map(|def| def.id);
        if let Some(def_id) = build_def {
            out.insert(alias, def_id);
        }
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn collect_boundary_call_links(
    source_id: SourceId,
    file: &SourceFile,
    exprs: &[BridgeExpr],
    resolved_refs: &[BridgeResolvedRef],
    local_bindings: &[BridgeLocalBinding],
    local_closure_inits: &[BridgeLocalClosureInit],
    call_sites: &[BridgeCallSite],
) -> (
    Vec<BridgeBoundaryArgBinding>,
    Vec<BridgeBoundaryArgClosure>,
    Vec<BridgeLocalBoundaryInit>,
) {
    let by_span = expr_id_by_span(exprs);
    let by_id = exprs
        .iter()
        .map(|expr| (expr.id, expr))
        .collect::<HashMap<_, _>>();
    let local_binding_by_def = local_bindings
        .iter()
        .map(|binding| (binding.def_id, binding.id))
        .collect::<HashMap<_, _>>();
    let closure_by_binding = local_closure_inits
        .iter()
        .map(|init| (init.binding, init.source.clone()))
        .collect::<HashMap<_, _>>();
    let boundary_calls = call_sites
        .iter()
        .filter(|site| matches!(site.kind, BridgeCallSiteKind::StdlibBoundary(_)))
        .map(|site| site.expr)
        .collect::<HashSet<_>>();

    let mut arg_bindings = Vec::new();
    let mut arg_closures = Vec::new();
    let mut boundary_inits = Vec::new();

    for call in file
        .syntax()
        .descendants()
        .filter_map(vx_parser::CallExpr::cast)
    {
        let call_span = SourceSpan::new(source_id, call.syntax().text_range());
        let Some(call_expr) = by_span.get(&call_span).copied() else {
            continue;
        };
        if !boundary_calls.contains(&call_expr) {
            continue;
        }
        let Some(body) = by_id.get(&call_expr).and_then(|expr| expr.body) else {
            continue;
        };

        if let Some(let_stmt) = call.syntax().ancestors().find_map(vx_parser::LetStmt::cast)
            && let Some(let_expr) = let_stmt.expr()
            && let_expr.syntax() == call.syntax()
            && let Some(name) = let_stmt.name()
            && let Some(binding) = local_bindings.iter().find(|binding| {
                binding.kind == BridgeLocalBindingKind::Let
                    && binding.body == body
                    && binding.name == name.text()
            })
        {
            boundary_inits.push(BridgeLocalBoundaryInit {
                body,
                binding: binding.id,
                call_expr,
            });
        }

        let mut arg_exprs = call.positional_args();
        arg_exprs.extend(call.keyword_args().filter_map(|arg| arg.expr()));

        for arg_expr in arg_exprs {
            let arg_span = SourceSpan::new(source_id, arg_expr.syntax().text_range());
            let mut bindings_for_arg = resolved_refs
                .iter()
                .filter(|resolved| {
                    resolved.span.source == arg_span.source
                        && arg_span.range.contains_range(resolved.span.range)
                })
                .filter_map(|resolved| {
                    let def = resolved.target.as_ref().ok()?;
                    local_binding_by_def.get(&def.id).copied()
                })
                .collect::<Vec<_>>();
            bindings_for_arg.sort_by_key(|binding| binding.0);
            bindings_for_arg.dedup();

            for binding in bindings_for_arg {
                arg_bindings.push(BridgeBoundaryArgBinding {
                    body,
                    call_expr,
                    binding,
                });
                if let Some(closure_source) = closure_by_binding.get(&binding).cloned() {
                    arg_closures.push(BridgeBoundaryArgClosure {
                        body,
                        call_expr,
                        closure_source,
                    });
                }
            }

            let arg_source_span = SourceSpan::new(source_id, arg_expr.syntax().text_range());
            let direct_closure = match arg_expr {
                Expr::Closure(_) => {
                    by_span
                        .get(&arg_source_span)
                        .copied()
                        .map(|closure_expr| SourceExpr {
                            body,
                            expr: closure_expr,
                        })
                }
                _ => None,
            };
            if let Some(closure_source) = direct_closure {
                arg_closures.push(BridgeBoundaryArgClosure {
                    body,
                    call_expr,
                    closure_source,
                });
            }
        }
    }

    arg_bindings.sort_by_key(|arg| (arg.body.0, arg.call_expr.0, arg.binding.0));
    arg_bindings.dedup();
    arg_closures.sort_by_key(|arg| {
        (
            arg.body.0,
            arg.call_expr.0,
            arg.closure_source.body.0,
            arg.closure_source.expr.0,
        )
    });
    arg_closures.dedup();
    boundary_inits.sort_by_key(|init| (init.body.0, init.binding.0, init.call_expr.0));
    boundary_inits.dedup();

    (arg_bindings, arg_closures, boundary_inits)
}
