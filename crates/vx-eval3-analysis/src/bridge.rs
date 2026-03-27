use std::collections::{BTreeMap, HashMap};

use vx_parser::{AstNode, Expr, Parsed, SourceFile, SyntaxError};
use vx_resolve::{
    ImportGraph, NameResolution, ResolveError, SourceFileRef, SourceId, SourceSpan,
    TypeCheckResult, check_capabilities, collect_types_for_sources, resolve_in_program,
    stdlib_sources, type_check,
};
use vx_types::Ty;

use crate::{BodyId, ExprId};

#[derive(Debug, Clone)]
pub struct BridgeBody {
    pub id: BodyId,
    pub name: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeExprKind {
    Call,
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
    pub target: Result<vx_resolve::DefId, ResolveError>,
}

#[derive(Debug, Clone)]
pub struct SingleSourceBridge {
    pub source_id: SourceId,
    pub parsed: Parsed<SourceFile>,
    pub bodies: Vec<BridgeBody>,
    pub exprs: Vec<BridgeExpr>,
    pub resolved_refs: Vec<BridgeResolvedRef>,
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

        let bodies = collect_bodies(source_id, &file);
        let exprs = collect_exprs(source_id, &file, &bodies);
        let resolved_refs = collect_resolved_refs(&resolution);
        let expr_types = collect_expr_types(&exprs, &type_check);

        Self {
            source_id,
            parsed,
            bodies,
            exprs,
            resolved_refs,
            expr_types,
            parse_errors,
            resolve_errors: resolution.errors,
            type_errors: type_check.errors,
            cap_errors: cap_check.errors,
        }
    }
}

fn collect_bodies(source_id: SourceId, file: &SourceFile) -> Vec<BridgeBody> {
    let mut bodies = Vec::new();

    for fn_decl in file.items().filter_map(|item| match item {
        vx_parser::Item::FnDecl(decl) => Some(decl),
        _ => None,
    }) {
        if let Some(name) = fn_decl.name() {
            let body_id = BodyId(bodies.len() as u32);
            bodies.push(BridgeBody {
                id: body_id,
                name: name.text().to_owned(),
                span: SourceSpan::new(source_id, fn_decl.syntax().text_range()),
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
            target: resolved.target.clone().map(|def| def.id),
        })
        .collect()
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
