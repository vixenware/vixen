//! Type-check a parsed `@grammar[ … ]` CLI literal against its tool grammar —
//! the command-line analogue of `sqlx::query!` checking SQL against a schema.
//!
//! This is the engine side of the check, runnable with zero Vixen evaluation:
//! resolve the grammar, validate the static arguments (unknown flag, value not
//! in a fixed domain, flag illegal in the command's mode), and, for every
//! interpolation hole, resolve the **expected type** from the grammar position
//! it lands on. Unifying that expectation with the spliced expression's actual
//! Vixen type is the compiler's job, once the type system is consulted here; the
//! [`HoleExpectation`]s this produces are precisely its input.

use command_model::{ArgRole, GrammarSet, Kind, Lint, ValueDomain};
use vx_parser::{AstNode, CliArg, CliInterp, CliLiteralExpr, SyntaxKind};

/// A problem found checking a CLI literal against its grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliDiagnostic {
    /// `@name[ … ]` where `name` resolves to no grammar in scope.
    UnknownGrammar { name: String },
    /// A `-`-leading argument the grammar does not define.
    UnknownFlag { token: String },
    /// A flag used in a mode it is not legal in.
    IllegalInMode { flag: String, mode: String },
    /// A static value outside the flag's fixed value set.
    ValueNotAllowed {
        flag: String,
        value: String,
        allowed: Vec<String>,
    },
}

/// The expected type of one interpolation hole, read off the grammar position it
/// occupies. The compiler unifies the spliced expression's Vixen type against
/// this; here we surface what the grammar demands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoleExpectation {
    /// The spliced source — a variable name for `$x`, an expression for `${ … }`.
    pub source: String,
    /// The open domain tag of the position (e.g. `"include"`, `"output"`).
    pub role: String,
    /// The cache/IO class — what `exec` would do with the value (mount, …).
    pub kind: Kind,
    /// The value space the hole must satisfy (path, one-of, …).
    pub value: ValueDomain,
}

/// The result of checking a CLI literal: diagnostics plus the typed holes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked {
    pub diagnostics: Vec<CliDiagnostic>,
    pub holes: Vec<HoleExpectation>,
}

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

/// Check `lit` against the grammar it names, resolved from `set`.
pub fn check_cli_literal(lit: &CliLiteralExpr, set: &GrammarSet) -> Checked {
    let mut out = Checked::default();
    let Some(name) = lit.grammar().map(|t| t.text().to_string()) else {
        return out;
    };
    let Some(grammar) = set.get(&name) else {
        out.diagnostics.push(CliDiagnostic::UnknownGrammar { name });
        return out;
    };

    // Lower the literal to a synthetic argv: static text verbatim, each hole a
    // unique sentinel we can recognise after parsing. `holes[i]` is the source
    // spliced at sentinel `i`.
    let mut argv = vec![name.clone()];
    let mut holes: Vec<String> = Vec::new();
    for arg in lit.args() {
        argv.push(synthesize_arg(&arg, &mut holes));
    }

    // Static diagnostics the engine already computes (unknown flag, mode).
    for lint in &set.canonicalize(&name, &argv).lints {
        match lint {
            Lint::UnknownFlag { token } if !contains_hole(token) => {
                out.diagnostics.push(CliDiagnostic::UnknownFlag {
                    token: token.clone(),
                });
            }
            Lint::IllegalInMode { flag, mode } => {
                out.diagnostics.push(CliDiagnostic::IllegalInMode {
                    flag: flag.clone(),
                    mode: mode.clone(),
                });
            }
            _ => {}
        }
    }

    // Per-argument: a hole resolves to an expectation; a static value is checked
    // against its domain.
    let parsed = command_model::parse_command(grammar, &argv);
    for arg in &parsed.args {
        match &arg.role {
            ArgRole::Flag {
                canonical,
                value: Some(value),
                kind,
                role,
            } => {
                let domain = grammar
                    .flags
                    .iter()
                    .find(|f| &f.canonical == canonical)
                    .map(|f| f.value.clone())
                    .unwrap_or(ValueDomain::Any);
                if let Some(idx) = hole_index(value) {
                    out.holes.push(HoleExpectation {
                        source: holes.get(idx).cloned().unwrap_or_default(),
                        role: role.clone(),
                        kind: *kind,
                        value: domain,
                    });
                } else if let ValueDomain::OneOf { values } = &domain
                    && !values.contains(value)
                {
                    out.diagnostics.push(CliDiagnostic::ValueNotAllowed {
                        flag: canonical.clone(),
                        value: value.clone(),
                        allowed: values.clone(),
                    });
                }
            }
            ArgRole::Positional { name, kind, value } => {
                if let Some(idx) = hole_index(value) {
                    let role = grammar
                        .positionals
                        .iter()
                        .find(|p| &p.name == name)
                        .map(|p| p.role.clone())
                        .unwrap_or_default();
                    out.holes.push(HoleExpectation {
                        source: holes.get(idx).cloned().unwrap_or_default(),
                        role,
                        kind: *kind,
                        value: ValueDomain::Any,
                    });
                }
            }
            _ => {}
        }
    }

    out
}

/// Build the synthetic-argv string for one argument, pushing each hole's source
/// into `holes` and standing it in with a sentinel.
fn synthesize_arg(arg: &CliArg, holes: &mut Vec<String>) -> String {
    let mut s = String::new();
    for child in arg.syntax().children_with_tokens() {
        if let Some(token) = child.as_token() {
            match token.kind() {
                SyntaxKind::CLI_WORD => s.push_str(token.text()),
                SyntaxKind::STRING => s.push_str(unquote(token.text())),
                SyntaxKind::CLI_INTERP_VAR => {
                    s.push_str(&sentinel(holes.len()));
                    holes.push(token.text().to_string());
                }
                _ => {}
            }
        } else if let Some(node) = child.as_node()
            && node.kind() == SyntaxKind::CLI_INTERP
        {
            s.push_str(&sentinel(holes.len()));
            let source = CliInterp::cast(node.clone())
                .and_then(|h| h.expr())
                .map(|e| e.syntax().text().to_string().trim().to_string())
                .unwrap_or_default();
            holes.push(source);
        }
    }
    s
}

/// If `value` carries a hole sentinel, the index of the hole it stands for.
fn hole_index(value: &str) -> Option<usize> {
    let start = value.find(MARK)? + MARK.len_utf8();
    let rest = &value[start..];
    let end = rest.find(MARK)?;
    rest[..end].parse().ok()
}

/// Strip surrounding double quotes from a lexed string token (no escape
/// processing — sufficient for domain checks).
fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text)
}
