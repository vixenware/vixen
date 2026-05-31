//! CLI-literal type checking (`@cc[ … ]`).
//!
//! The command-line analogue of `sqlx::query!`: validate the literal against the
//! tool grammar the name resolves to — unknown flag, value outside a fixed
//! domain, flag illegal in the command's mode — all at compile time, with zero
//! Vixen evaluation. The grammar reaches us as a [`command_model::GrammarSet`]
//! the compile driver loaded from the imported grammar package (keyed on the
//! arena by import spec); we find it through the binding `@name` resolves to.
//!
//! Crucially, **holes are typed by position**: each `$x` / `${ … }` sits at a
//! grammar position whose [`ValueDomain`]/[`Kind`] becomes the *expected type*
//! of the spliced expression — a path position demands a `Path`, a fixed-value
//! position demands a `String`. A hole whose Vixen type doesn't match its
//! position is a type error. This is where Vixen's own type system meets the
//! grammar's value domains.

use command_model::{ArgRole, Arity, Grammar, GrammarSet, Kind, Lint, Policy, ValueDomain};
use vx_parser::SourceSpan;
use vx_types::{Scalar, TypeId};

use crate::arena::VirArena;
use crate::diagnostic::{CliDiagnosticKind, CliHoleExpect, IrDiagnostic};
use crate::expr::{CliLiteralExpr, StringPart};
use crate::scope::{self, DefTarget};
use crate::{ExprId, ScopeId};

use super::TypeChecker;

// A private-use marker bracketing each interpolation hole inside the synthetic
// argv, so the engine's parser tags the hole's position for us. `\u{1}` cannot
// occur in real source.
const MARK: char = '\u{1}';

fn sentinel(n: usize) -> String {
    format!("{MARK}{n}{MARK}")
}

fn contains_hole(value: &str) -> bool {
    value.contains(MARK)
}

/// Every hole index a sentinel-bearing value carries, in order.
fn hole_indices(value: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find(MARK) {
        let after = &rest[start + MARK.len_utf8()..];
        let Some(end) = after.find(MARK) else { break };
        if let Ok(n) = after[..end].parse() {
            out.push(n);
        }
        rest = &after[end + MARK.len_utf8()..];
    }
    out
}

/// What a hole at a given grammar position must be.
struct HoleReq {
    expect: CliHoleExpect,
    /// A repeatable position (repeatable flag policy / variadic positional) also
    /// accepts an `Array` of the element type (splatting).
    repeatable: bool,
}

pub(crate) fn infer_cli_literal(
    tc: &mut TypeChecker,
    expr_id: ExprId,
    cli: &CliLiteralExpr,
) -> TypeId {
    let scope = tc.arena[expr_id].scope;

    // Lower the literal to a synthetic argv: static text verbatim, each hole a
    // unique sentinel the engine's parser can tag. Record each hole's spliced
    // expression so we can infer + position-check it.
    let mut argv = vec![cli.grammar.clone()];
    let mut hole_exprs: Vec<ExprId> = Vec::new();
    for arg in &cli.args {
        let mut s = String::new();
        for piece in &arg.pieces {
            match piece {
                StringPart::Text(t) => s.push_str(t),
                StringPart::Expr(id) => {
                    s.push_str(&sentinel(hole_exprs.len()));
                    hole_exprs.push(*id);
                }
            }
        }
        argv.push(s);
    }

    // Holes are real, checked sub-expressions — infer them up front (mutable),
    // so the grammar phase below can run entirely on an immutable arena borrow.
    let hole_tys: Vec<TypeId> = hole_exprs.iter().map(|&id| tc.infer_expr(id)).collect();

    // Resolve the grammar `@name` refers to. All grammar work is immutable on
    // the arena; structural diagnostics + per-hole requirements are collected
    // owned so the borrow releases before we type-check holes / push.
    let mut diags: Vec<(SourceSpan, CliDiagnosticKind)> = Vec::new();
    let mut hole_reqs: Vec<(usize, HoleReq)> = Vec::new();
    // The `@[ ${cmd} … ]` form carries no grammar name — its grammar comes from the
    // leading hole's type, resolved in a later pass (the holes are still inferred
    // above). Only the named `@grammar[ … ]` form validates against a named grammar.
    if !cli.grammar.is_empty() {
        match resolve_grammar_set(tc.arena, &cli.grammar, scope) {
            Some(set) if set.get(&cli.grammar).is_some() => {
                let grammar = set.get(&cli.grammar).unwrap();
                check_structural(set, grammar, &cli.grammar, &argv, cli, &mut diags);
                hole_reqs = collect_hole_reqs(grammar, &argv);
            }
            _ => {
                diags.push((
                    cli.grammar_span,
                    CliDiagnosticKind::UnknownGrammar {
                        name: cli.grammar.clone(),
                    },
                ));
            }
        }
    }

    // Type each hole against the position it lands on.
    for (hole_idx, req) in &hole_reqs {
        let Some(&hty) = hole_tys.get(*hole_idx) else {
            continue;
        };
        if let Some(kind) = check_hole_type(tc, hty, req) {
            let span = tc.arena[hole_exprs[*hole_idx]].span;
            diags.push((span, kind));
        }
    }

    for (span, kind) in diags {
        tc.push_diagnostic(IrDiagnostic::Cli { span, kind });
    }

    // The literal's own value (a `Command`) is not modeled in the type system
    // yet, so it occupies no concrete type.
    tc.poison()
}

/// Find the [`GrammarSet`] that `@name` resolves to: `name` must be a binding in
/// scope brought in by importing a grammar package, and that package's set is
/// recorded on the arena under the import spec.
fn resolve_grammar_set<'a>(
    arena: &'a VirArena,
    name: &str,
    scope: ScopeId,
) -> Option<&'a GrammarSet> {
    let DefTarget::Binding(bid) = scope::lookup_at(arena, name, scope, u32::MAX)? else {
        return None;
    };
    let origin = arena[bid].import_origin.as_ref()?;
    arena.grammars.get(&origin.spec)
}

/// Structural checks on the static argv: unknown flag, flag illegal in the
/// active mode, and static values outside a fixed (`OneOf`) domain. Holes
/// (sentinel-bearing tokens) are skipped here — they are typed by position.
fn check_structural(
    set: &GrammarSet,
    grammar: &Grammar,
    name: &str,
    argv: &[String],
    cli: &CliLiteralExpr,
    diags: &mut Vec<(SourceSpan, CliDiagnosticKind)>,
) {
    for lint in &set.canonicalize(name, argv).lints {
        match lint {
            Lint::UnknownFlag { token } if !contains_hole(token) => {
                diags.push((
                    arg_span_for(cli, token),
                    CliDiagnosticKind::UnknownFlag {
                        token: token.clone(),
                    },
                ));
            }
            Lint::IllegalInMode { flag, mode } => {
                diags.push((
                    arg_span_for(cli, flag),
                    CliDiagnosticKind::IllegalInMode {
                        flag: flag.clone(),
                        mode: mode.clone(),
                    },
                ));
            }
            _ => {}
        }
    }

    let parsed = command_model::parse_command(grammar, argv);
    for arg in &parsed.args {
        if let ArgRole::Flag {
            canonical,
            value: Some(value),
            ..
        } = &arg.role
            && !contains_hole(value)
            && let Some(ValueDomain::OneOf { values }) = grammar
                .flags
                .iter()
                .find(|f| &f.canonical == canonical)
                .map(|f| &f.value)
            && !values.contains(value)
        {
            diags.push((
                arg_span_for(cli, value),
                CliDiagnosticKind::ValueNotAllowed {
                    flag: canonical.clone(),
                    value: value.clone(),
                    allowed: values.clone(),
                },
            ));
        }
    }
}

/// Read each interpolation hole's expected type off the grammar position it
/// occupies: a flag value's [`ValueDomain`], or a positional's [`Kind`].
fn collect_hole_reqs(grammar: &Grammar, argv: &[String]) -> Vec<(usize, HoleReq)> {
    let mut reqs = Vec::new();
    for arg in &command_model::parse_command(grammar, argv).args {
        match &arg.role {
            ArgRole::Flag {
                canonical,
                value: Some(value),
                ..
            } => {
                let holes = hole_indices(value);
                if holes.is_empty() {
                    continue;
                }
                let flag = grammar.flags.iter().find(|f| &f.canonical == canonical);
                let expect = match flag.map(|f| &f.value) {
                    Some(ValueDomain::Path { .. }) => CliHoleExpect::Path,
                    _ => CliHoleExpect::Text,
                };
                let repeatable = flag.is_some_and(|f| is_repeatable(&f.policy));
                for h in holes {
                    reqs.push((h, HoleReq { expect, repeatable }));
                }
            }
            ArgRole::Positional { name, value, .. } => {
                let holes = hole_indices(value);
                if holes.is_empty() {
                    continue;
                }
                let pos = grammar.positionals.iter().find(|p| &p.name == name);
                let expect = match pos.map(|p| &p.kind) {
                    Some(Kind::Text) | None => CliHoleExpect::Text,
                    Some(_) => CliHoleExpect::Path,
                };
                let repeatable = pos.is_some_and(|p| matches!(p.arity, Arity::Variadic));
                for h in holes {
                    reqs.push((h, HoleReq { expect, repeatable }));
                }
            }
            _ => {}
        }
    }
    reqs
}

/// A policy under which a flag may legally appear more than once (so an
/// `Array`-valued hole at it splats into repeats).
fn is_repeatable(policy: &Policy) -> bool {
    matches!(
        policy,
        Policy::DedupOrdered
            | Policy::DedupUnordered
            | Policy::OrderedKeepAll
            | Policy::KeyedLastWins
    )
}

/// Check one hole's inferred type against its position's expectation, returning
/// a diagnostic kind on mismatch. A repeatable position also accepts an `Array`
/// of the element type.
fn check_hole_type(tc: &mut TypeChecker, hty: TypeId, req: &HoleReq) -> Option<CliDiagnosticKind> {
    if tc.types.env.get_ty(hty).is_poison() {
        return None;
    }
    let elems: Vec<TypeId> = match req.expect {
        CliHoleExpect::Path => vec![tc.nominal_ty("Path"), tc.nominal_ty("ExecPath")],
        CliHoleExpect::Text => vec![tc.types.env.intern_scalar(Scalar::String)],
    };
    if elems.iter().any(|&e| tc.types_compatible(hty, e)) {
        return None;
    }
    if req.repeatable {
        for &e in &elems {
            let arr = tc.types.env.intern_array(e);
            if tc.types_compatible(hty, arr) {
                return None;
            }
        }
    }
    Some(CliDiagnosticKind::HoleType {
        expected: req.expect,
        found: hty,
    })
}

/// The span of the source argument whose static text carries `token` (an exact
/// match preferred over a substring), falling back to the grammar name when no
/// argument matches.
fn arg_span_for(cli: &CliLiteralExpr, token: &str) -> SourceSpan {
    let static_text = |arg: &crate::expr::CliArgIr| -> String {
        arg.pieces
            .iter()
            .filter_map(|p| match p {
                StringPart::Text(t) => Some(t.as_str()),
                StringPart::Expr(_) => None,
            })
            .collect()
    };
    if let Some(arg) = cli.args.iter().find(|a| static_text(a) == token) {
        return arg.span;
    }
    if let Some(arg) = cli.args.iter().find(|a| static_text(a).contains(token)) {
        return arg.span;
    }
    cli.grammar_span
}
