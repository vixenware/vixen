//! Backtick-invocation type checking — tier-1 validation (`r[exec.validate.static]`).
//!
//! The command-line analogue of `sqlx::query!`: validate `` `$tool args…` `` against
//! the **command** the leading tool carries — unknown flag, value outside a fixed
//! domain, flag illegal in the command's mode — all at compile time, with zero
//! observer/exec evaluation. The grammar reaches us as a [`command_model::GrammarSet`]
//! the compile driver loaded from the imported grammar package (keyed on the arena by
//! import spec); we find it by **const-evaluating** `tool.command` to a coordinate
//! `(package, name)` (`const_command_coord`) — the command is a provider-independent
//! constant, so this works wherever the tool is statically determined.
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

use vx_ir::BindingId;
use vx_ir::ExprId;
use vx_ir::diagnostic::{CliDiagnosticKind, CliHoleExpect, IrDiagnostic};
use vx_ir::expr::{CliLiteralExpr, ExprKind, FieldKey, RecordField, StringPart};
use vx_ir::scope::DefTarget;
use vx_ir::stmt::StmtKind;

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
    _expr_id: ExprId,
    cli: &CliLiteralExpr,
) -> TypeId {
    // The leading argument is the **tool**; the rest are its grammar-checked
    // arguments (`r[exec.invocation]`). Infer the lead's hole(s) for their type, then
    // validate only the tokens after it — against the command the tool carries.
    let Some((lead, rest)) = cli.args.split_first() else {
        return tc.nominal_ty("Tree");
    };
    for piece in &lead.pieces {
        if let StringPart::Expr(id) = piece {
            tc.infer_expr(*id);
        }
    }
    // A single `$tool` hole is a const-evaluable tool; a multi-piece lead is not.
    let tool_expr = match lead.pieces.as_slice() {
        [StringPart::Expr(id)] => Some(*id),
        _ => None,
    };

    // Lower the argument tokens (after the tool) to a synthetic argv: static text
    // verbatim, each hole a unique sentinel. argv[0] is the program slot, filled with
    // the grammar's name once the command is const-evaluated.
    let mut arg_strs: Vec<String> = Vec::new();
    let mut hole_exprs: Vec<ExprId> = Vec::new();
    for arg in rest {
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
        arg_strs.push(s);
    }

    // Holes are real, checked sub-expressions — infer them up front (mutable),
    // so the grammar phase below can run entirely on an immutable arena borrow.
    let hole_tys: Vec<TypeId> = hole_exprs.iter().map(|&id| tc.infer_expr(id)).collect();

    // Const-evaluate the tool's `command` to a grammar coordinate
    // (`r[exec.validate.static]`) and validate the argument tokens against it. Where
    // the command isn't statically determined — a tool parameter, a runtime branch —
    // `const_command_coord` returns `None` and the invocation is not grammar-checked
    // ("generic over some tool"). Diagnostics + per-hole requirements are collected
    // owned so the immutable arena borrow releases before we type-check holes / push.
    let mut diags: Vec<(SourceSpan, CliDiagnosticKind)> = Vec::new();
    let mut hole_reqs: Vec<(usize, HoleReq)> = Vec::new();
    if let Some(tx) = tool_expr
        && let Some((spec, name)) = const_command_coord(tc, tx)
    {
        let tool_span = tc.arena[tx].span;
        if let Some(set) = tc.arena.grammars.get(&spec) {
            if let Some(grammar) = set.get(&name) {
                let mut argv = vec![name.clone()];
                argv.extend(arg_strs.iter().cloned());
                check_structural(set, grammar, &name, &argv, cli, &mut diags);
                hole_reqs = collect_hole_reqs(grammar, &argv);
            } else {
                // The package is loaded but has no such grammar — a genuine typo.
                diags.push((tool_span, CliDiagnosticKind::UnknownGrammar { name }));
            }
        }
        // A coordinate whose package isn't loaded is a loading gap, not a user error;
        // leave the invocation unchecked rather than reporting a phantom.
    }

    // Type each hole against the position it lands on.
    for (hole_idx, req) in &hole_reqs {
        let Some(&hty) = hole_tys.get(*hole_idx) else {
            continue;
        };
        let hole_expr = hole_exprs[*hole_idx];
        // A `Path` position accepts a plain `String`: paths are strings, so the
        // hole coerces (String→Path) in place — the same checking-mode coercion
        // as any other argument position. Try the scalar form, then `Array[Path]`
        // for a repeatable position; if neither coerces, fall back to the
        // structural check (which still accepts `ExecPath` / arrays of it and
        // reports a genuine mismatch).
        if matches!(req.expect, CliHoleExpect::Path) {
            let path_ty = tc.nominal_ty("Path");
            if tc.coerce_to(hole_expr, path_ty) {
                continue;
            }
            if req.repeatable {
                let arr = tc.types.env.intern_array(path_ty);
                if tc.coerce_to(hole_expr, arr) {
                    continue;
                }
            }
        }
        if let Some(kind) = check_hole_type(tc, hty, req) {
            let span = tc.arena[hole_expr].span;
            diags.push((span, kind));
        }
    }

    for (span, kind) in diags {
        tc.push_diagnostic(IrDiagnostic::Cli { span, kind });
    }

    // A bare invocation runs with the default observer `run.ok.out`
    // (`r[exec.observer.simple]`), so its value is the success-gated output `Tree`. A
    // `with { observer: … }` overrides this with the observer's return type (handled
    // in `infer_where`).
    tc.nominal_ty("Tree")
}

/// Const-evaluate the leading tool's `command` to a grammar coordinate
/// `(package, name)` — statically, by tracing the tool's construction
/// (`r[exec.validate.static]`). The command is provider-independent, so this works
/// through `let`/field-access wherever the tool is statically determined; it returns
/// `None` for a tool parameter or a runtime branch (no statically-known command).
fn const_command_coord(tc: &TypeChecker, tool_expr: ExprId) -> Option<(String, String)> {
    let fields = trace_record_fields(tc, tool_expr)?;
    let command_expr = fields.iter().find(|f| f.name == "command")?.value;
    grammar_coord_from_field(tc, command_expr)
}

/// Trace `expr` to the fields of the record it is built from: a record literal
/// directly, or a `let`-binding whose initializer is one (following the binding to
/// its `let` statement). Other shapes (a parameter, a call) are not const-records.
fn trace_record_fields(tc: &TypeChecker, expr: ExprId) -> Option<Vec<RecordField>> {
    match &tc.arena[expr].kind {
        ExprKind::Record(rec) => Some(rec.fields.clone()),
        ExprKind::NameRef(nr) => {
            let DefTarget::Binding(bid) = nr.resolved? else {
                return None;
            };
            let init = let_init_for_binding(tc, bid)?;
            trace_record_fields(tc, init)
        }
        _ => None,
    }
}

/// A grammar coordinate `(package, name)` from a `g.cc` field access on an imported
/// grammar package: the receiver `g` resolves to an `import` binding whose origin
/// carries the package spec, and the field name is the grammar's name.
fn grammar_coord_from_field(tc: &TypeChecker, expr: ExprId) -> Option<(String, String)> {
    let ExprKind::Field(f) = &tc.arena[expr].kind else {
        return None;
    };
    let FieldKey::Name(grammar_name) = &f.field else {
        return None;
    };
    let ExprKind::NameRef(nr) = &tc.arena[f.receiver].kind else {
        return None;
    };
    let DefTarget::Binding(bid) = nr.resolved? else {
        return None;
    };
    let spec = tc.arena[bid].import_origin.as_ref()?.spec.clone();
    Some((spec, grammar_name.clone()))
}

/// The initializer expression of the `let` statement that binds `binding`.
fn let_init_for_binding(tc: &TypeChecker, binding: BindingId) -> Option<ExprId> {
    tc.arena.stmts.iter().find_map(|stmt| match &stmt.kind {
        StmtKind::Let(let_) if let_binds_pattern(tc, let_.pattern, binding) => Some(let_.init),
        _ => None,
    })
}

/// Whether `pattern` is the simple binding pattern for `binding`.
fn let_binds_pattern(tc: &TypeChecker, pattern: vx_ir::PatternId, binding: BindingId) -> bool {
    matches!(
        &tc.arena[pattern].kind,
        vx_ir::pattern::PatternKind::Binding(bp) if bp.binding == binding
    )
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
    let static_text = |arg: &vx_ir::expr::CliArgIr| -> String {
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
