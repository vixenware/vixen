use std::collections::{BTreeMap, HashMap};

use vx_parser::{AstNode, Expr, Parsed, SourceFile, SyntaxError};
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

#[derive(Debug, Clone)]
pub struct SingleSourceBridge {
    pub source_id: SourceId,
    pub parsed: Parsed<SourceFile>,
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

        let mut imports = ImportGraph::new();
        let mut sources = vec![SourceFileRef {
            id: source_id,
            file: file.clone(),
        }];
        for (index, stdlib) in stdlib_sources().iter().enumerate() {
            let parsed_stdlib = vx_parser::parse_source_file(stdlib.text);
            let stdlib_id = SourceId(index as u32 + 1);
            imports.add_import(source_id, stdlib_id);
            sources.push(SourceFileRef {
                id: stdlib_id,
                file: parsed_stdlib.tree(),
            });
        }

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

        let bodies = collect_bodies(source_id, &file, &resolution);
        let exprs = collect_exprs(source_id, &file, &bodies);
        let resolved_refs = collect_resolved_refs(&resolution);
        let products = collect_products(source_id, &file, &exprs);
        let projections = collect_projections(source_id, &file, &exprs);
        let binding_products = collect_binding_products(source_id, &file, &bodies);
        let local_bindings = collect_local_bindings(source_id, &resolution, &bodies);
        let local_uses = collect_local_uses(&resolution, &exprs, &local_bindings);
        let local_closure_inits =
            collect_local_closure_inits(source_id, &file, &exprs, &local_bindings, &bodies);
        let call_sites = collect_call_sites(source_id, &file, &exprs, &resolved_refs);
        let expr_types = collect_expr_types(&exprs, &type_check);

        Self {
            source_id,
            parsed,
            bodies,
            exprs,
            resolved_refs,
            products,
            projections,
            binding_products,
            local_bindings,
            local_uses,
            local_closure_inits,
            call_sites,
            expr_types,
            parse_errors,
            resolve_errors: resolution.errors,
            type_errors: type_check.errors,
            cap_errors: cap_check.errors,
        }
    }
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
            if expr.span.range.contains_range(span.range) {
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
    let param_spans = bodies
        .iter()
        .flat_map(|body| body.params.iter().map(move |param| (body.id, param.span)))
        .collect::<Vec<_>>();

    let mut entries = resolution
        .scopes
        .all_definitions_in(source_id)
        .into_iter()
        .filter_map(|def| {
            let body = body_for_range(def.name_span.range, bodies)?;
            let kind = match def.kind {
                DefKind::Binding => BridgeLocalBindingKind::Let,
                DefKind::Parameter => {
                    if !param_spans
                        .iter()
                        .any(|(param_body, span)| *param_body == body && *span == def.name_span)
                    {
                        return None;
                    }
                    BridgeLocalBindingKind::Parameter
                }
                _ => return None,
            };

            Some((body, def.name_span, def.name.clone(), kind, def.id))
        })
        .collect::<Vec<_>>();

    entries.sort_by_key(|(body, span, name, kind, _)| {
        let kind_rank = match kind {
            BridgeLocalBindingKind::Parameter => 0u8,
            BridgeLocalBindingKind::Let => 1,
        };
        (
            body.0,
            u32::from(span.range.start()),
            u32::from(span.range.end()),
            kind_rank,
            name.clone(),
        )
    });

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
            let callee_span = SourceSpan::new(source_id, callee.syntax().text_range());

            let resolved = resolved_refs
                .iter()
                .filter(|resolved| callee_span.range.contains_range(resolved.span.range))
                .min_by_key(|resolved| {
                    let len = u32::from(resolved.span.range.end())
                        - u32::from(resolved.span.range.start());
                    (len, u32::from(resolved.span.range.start()))
                });

            let kind = match resolved {
                Some(resolved) => {
                    if let Some(boundary_kind) = classify_runtime_boundary(source_id, resolved) {
                        BridgeCallSiteKind::StdlibBoundary(boundary_kind)
                    } else {
                        match resolved.target.as_ref().ok().map(|target| target.kind) {
                            Some(DefKind::Function) => BridgeCallSiteKind::DirectNamedFunction,
                            Some(DefKind::Binding) | Some(DefKind::Parameter) => {
                                BridgeCallSiteKind::LocalCallableBinding
                            }
                            _ => BridgeCallSiteKind::Other,
                        }
                    }
                }
                None => BridgeCallSiteKind::Other,
            };
            let target = resolved.and_then(|resolved| resolved.target.as_ref().ok().map(|d| d.id));

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
