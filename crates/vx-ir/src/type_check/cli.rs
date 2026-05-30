//! CLI-literal type checking (`@cc[ … ]`).
//!
//! The command-line analogue of `sqlx::query!`: validate the literal against the
//! tool grammar the name resolves to — unknown flag, value outside a fixed
//! domain, flag illegal in the command's mode — all at compile time, with zero
//! Vixen evaluation. The grammar reaches us as a [`command_model::GrammarSet`]
//! the compile driver loaded from the imported grammar package (keyed on the
//! arena by import spec); we find it through the binding `@name` resolves to.
//!
//! Interpolation holes (`$x` / `${ … }`) are inferred as real sub-expressions.
//! Reading each hole's *expected* type off the grammar position it lands on (so
//! the spliced expression is unified against what the grammar demands) is the
//! next layer; this one establishes the structural check and the seam.

use command_model::{ArgRole, Grammar, GrammarSet, Lint, ValueDomain};
use vx_parser::SourceSpan;
use vx_types::TypeId;

use crate::arena::VirArena;
use crate::diagnostic::{CliDiagnosticKind, IrDiagnostic};
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

pub(crate) fn infer_cli_literal(
    tc: &mut TypeChecker,
    expr_id: ExprId,
    cli: &CliLiteralExpr,
) -> TypeId {
    let scope = tc.arena[expr_id].scope;

    // Lower the literal to a synthetic argv: static text verbatim, each hole a
    // unique sentinel the engine's parser can tag. Holes are recorded as the
    // sub-expressions they splice, to be inferred once the grammar borrow ends.
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

    // Resolve the grammar `@name` refers to and check the static argv against
    // it. All grammar work is immutable on the arena; diagnostics are collected
    // owned so the arena borrow is released before we infer holes / push them.
    let mut diags: Vec<(SourceSpan, CliDiagnosticKind)> = Vec::new();
    match resolve_grammar_set(tc.arena, &cli.grammar, scope) {
        Some(set) if set.get(&cli.grammar).is_some() => {
            let grammar = set.get(&cli.grammar).unwrap();
            check_argv(set, grammar, &cli.grammar, &argv, cli, &mut diags);
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

    // Holes are real, checked sub-expressions regardless of grammar outcome.
    for &id in &hole_exprs {
        tc.infer_expr(id);
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

/// Check the synthetic `argv` against `grammar` (resolved from `set`), pushing a
/// `(span, kind)` for each problem. Holes (sentinel-bearing tokens) are skipped
/// here — they carry no statically-known value to validate.
fn check_argv(
    set: &GrammarSet,
    grammar: &Grammar,
    name: &str,
    argv: &[String],
    cli: &CliLiteralExpr,
    diags: &mut Vec<(SourceSpan, CliDiagnosticKind)>,
) {
    // Structural lints the canonicalizer already computes (unknown flag, mode).
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

    // Per-flag: a static value in a fixed domain must be a member of it.
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
