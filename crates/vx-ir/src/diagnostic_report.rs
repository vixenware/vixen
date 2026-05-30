//! Conversion from [`IrDiagnostic`] to [`margin::Report`].
//!
//! This is the last-mile conversion from the structured, string-free
//! [`IrDiagnostic`] enum into the rich, renderable [`margin::Report`] shape.
//! Strings appear here for the first time — sourced from a [`TypeEnv`]
//! (for type display) or from the diagnostic variant's structural data
//! (operator kinds, capability names, etc.). Nothing inside an
//! [`IrDiagnostic`] is text.
//!
//! Adding a new diagnostic variant: extend the `match` in
//! [`IrDiagnostic::to_report`] below.

use margin::{Annotation, AnnotationRole, Note, NoteKind, Report, Severity, Span};
use vx_parser::SourceSpan;
use vx_types::TypeEnv;

use crate::cap::CapSet;
use crate::diagnostic::{IrDiagnostic, MethodCandidate};
use crate::expr::BinOp;
use crate::scope::DefTarget;

impl IrDiagnostic {
    /// Render this diagnostic as a [`margin::Report`].
    ///
    /// All variants currently produce [`Severity::Error`]. The `env` is
    /// used to format any [`vx_types::Ty`] values carried by the variant.
    pub fn to_report(&self, env: &TypeEnv) -> Report {
        match self {
            IrDiagnostic::Unresolved { name } => Report {
                severity: Severity::Error,
                title: "unresolved name".to_string(),
                annotations: vec![primary(*name, Some("not found in scope".to_string()))],
                notes: Vec::new(),
                sections: Vec::new(),
            },

            IrDiagnostic::InvalidTarget { name, found } => {
                let message = match found {
                    DefTarget::Item(_) => {
                        "this name refers to a function but a value is expected".to_string()
                    }
                    DefTarget::Binding(_) => "this is a binding, not a function".to_string(),
                    DefTarget::Variant { .. } => "variant cannot be used here".to_string(),
                };
                Report {
                    severity: Severity::Error,
                    title: "invalid target".to_string(),
                    annotations: vec![primary(*name, Some(message))],
                    notes: Vec::new(),
                    sections: Vec::new(),
                }
            }

            IrDiagnostic::ArgTypeMismatch {
                arg,
                param,
                expected,
                found,
            } => Report {
                severity: Severity::Error,
                title: "argument type mismatch".to_string(),
                annotations: vec![
                    primary(
                        *arg,
                        Some(format!(
                            "expected `{}`, found `{}`",
                            env.get_ty(*expected).display_with_env(env),
                            env.get_ty(*found).display_with_env(env)
                        )),
                    ),
                    context(*param, Some("parameter declared here".to_string())),
                ],
                notes: Vec::new(),
                sections: Vec::new(),
            },

            IrDiagnostic::BinaryOpMismatch {
                op,
                op_kind,
                lhs,
                rhs,
                lhs_ty,
                rhs_ty,
            } => Report {
                severity: Severity::Error,
                title: "binary operator type mismatch".to_string(),
                annotations: vec![
                    primary(
                        *op,
                        Some(format!(
                            "`{}` cannot be applied to `{}` and `{}`",
                            op_kind_label(*op_kind),
                            env.get_ty(*lhs_ty).display_with_env(env),
                            env.get_ty(*rhs_ty).display_with_env(env)
                        )),
                    ),
                    context(
                        *lhs,
                        Some(format!("{}", env.get_ty(*lhs_ty).display_with_env(env))),
                    ),
                    context(
                        *rhs,
                        Some(format!("{}", env.get_ty(*rhs_ty).display_with_env(env))),
                    ),
                ],
                notes: Vec::new(),
                sections: Vec::new(),
            },

            IrDiagnostic::NoSuchMethod {
                method_call,
                receiver,
                receiver_ty,
                candidates,
            } => {
                let mut annotations = vec![
                    primary(
                        *method_call,
                        Some(format!(
                            "type `{}` has no method here",
                            env.get_ty(*receiver_ty).display_with_env(env)
                        )),
                    ),
                    context(
                        *receiver,
                        Some(format!(
                            "{}",
                            env.get_ty(*receiver_ty).display_with_env(env)
                        )),
                    ),
                ];
                for candidate in candidates {
                    annotations.push(candidate_annotation(candidate));
                }
                Report {
                    severity: Severity::Error,
                    title: "no such method".to_string(),
                    annotations,
                    notes: Vec::new(),
                    sections: Vec::new(),
                }
            }

            IrDiagnostic::UnusedExprStatement { expr } => Report {
                severity: Severity::Error,
                title: "unused expression statement".to_string(),
                annotations: vec![primary(
                    *expr,
                    Some("this expression's value is discarded".to_string()),
                )],
                notes: Vec::new(),
                sections: Vec::new(),
            },

            IrDiagnostic::CapabilityViolation {
                closure,
                slot,
                allowed,
                found,
            } => Report {
                severity: Severity::Error,
                title: "capability violation".to_string(),
                annotations: vec![
                    primary(
                        *closure,
                        Some(format!(
                            "inferred capabilities `{}` exceed allowed `{}`",
                            cap_set_display(*found),
                            cap_set_display(*allowed)
                        )),
                    ),
                    context(
                        *slot,
                        Some("slot declared with these allowed capabilities here".to_string()),
                    ),
                ],
                notes: vec![Note {
                    kind: NoteKind::Help,
                    text: format!(
                        "the closure uses {} but the slot only permits {}",
                        cap_set_display(*found),
                        cap_set_display(*allowed)
                    ),
                }],
                sections: Vec::new(),
            },

            IrDiagnostic::UnknownCapability { name } => Report {
                severity: Severity::Error,
                title: "unknown capability".to_string(),
                annotations: vec![primary(
                    *name,
                    Some("not a recognized capability".to_string()),
                )],
                notes: Vec::new(),
                sections: Vec::new(),
            },

            IrDiagnostic::Cli { span, kind } => {
                use crate::diagnostic::CliDiagnosticKind as K;
                let (title, message, notes) = match kind {
                    K::UnknownGrammar { name } => (
                        "unknown grammar".to_string(),
                        format!("`{name}` is not an imported grammar"),
                        vec![Note {
                            kind: NoteKind::Help,
                            text: format!("import a grammar package that exports `{name}`"),
                        }],
                    ),
                    K::UnknownFlag { token } => (
                        "unknown flag".to_string(),
                        format!("`{token}` is not defined by this grammar"),
                        Vec::new(),
                    ),
                    K::IllegalInMode { flag, mode } => (
                        "flag illegal in mode".to_string(),
                        format!("`{flag}` is not allowed in mode `{mode}`"),
                        Vec::new(),
                    ),
                    K::ValueNotAllowed {
                        flag,
                        value,
                        allowed,
                    } => (
                        "value not allowed".to_string(),
                        format!("`{value}` is not a valid value for `{flag}`"),
                        vec![Note {
                            kind: NoteKind::Help,
                            text: format!("allowed values: {}", allowed.join(", ")),
                        }],
                    ),
                };
                Report {
                    severity: Severity::Error,
                    title,
                    annotations: vec![primary(*span, Some(message))],
                    notes,
                    sections: Vec::new(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Annotation builders
// ---------------------------------------------------------------------------

/// A primary annotation — the main cursor for the diagnostic.
fn primary(span: SourceSpan, message: Option<String>) -> Annotation {
    Annotation {
        spans: vec![to_margin_span(span)],
        role: AnnotationRole::PrimaryLabel,
        syntax_class: None,
        message,
        priority: 100,
    }
}

/// A secondary "context" annotation — related sites that explain the error.
fn context(span: SourceSpan, message: Option<String>) -> Annotation {
    Annotation {
        spans: vec![to_margin_span(span)],
        role: AnnotationRole::SecondaryLabel,
        syntax_class: None,
        message,
        priority: 50,
    }
}

/// A method-candidate annotation — points at the candidate's declaration name.
fn candidate_annotation(candidate: &MethodCandidate) -> Annotation {
    Annotation {
        spans: vec![to_margin_span(candidate.name_span)],
        role: AnnotationRole::RelatedLabel,
        syntax_class: None,
        message: Some("candidate".to_string()),
        priority: 25,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert vx-ir's [`SourceSpan`] into margin's [`Span`].
///
/// vx-ir uses a `u32` source id; margin uses a string. We stringify.
fn to_margin_span(s: SourceSpan) -> Span {
    Span {
        source_id: margin::SourceId(s.source.0.to_string()),
        start: u32::from(s.range.start()) as usize,
        end: u32::from(s.range.end()) as usize,
    }
}

/// Source-level label for a binary operator.
fn op_kind_label(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Concat => "++",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Neq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Pipe => "|>",
    }
}

/// Render a [`CapSet`] as a `{cap1, cap2}` string.
fn cap_set_display(set: CapSet) -> String {
    let names = set.names();
    if names.is_empty() {
        return "{}".to_string();
    }
    let mut out = String::from("{");
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(name);
    }
    out.push('}');
    out
}
