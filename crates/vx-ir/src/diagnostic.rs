//! Structured diagnostics for VIR passes.
//!
//! [`IrDiagnostic`] is the unified diagnostic type produced by the resolve,
//! type-check, and capability-check passes. Each variant carries the
//! structural data needed to render a user-facing message — all names
//! carrying a source origin are `SourceSpan`s, all structural references
//! are IDs, and all operator/capability kinds are enums. No raw strings
//! live inside diagnostic variants.
//!
//! Rendering to a user-facing report happens at the consumer boundary
//! (e.g. LSP, CLI). [`IrDiagnostic::to_report`] (in
//! [`crate::diagnostic_report`]) converts a diagnostic into a renderable
//! [`margin::Report`]; consumers may also pattern-match on the variants
//! directly for structured handling.

use vx_parser::SourceSpan;
use vx_types::TypeId;

use crate::ItemId;
use crate::cap::CapSet;
use crate::expr::BinOp;
use crate::scope::DefTarget;

/// A diagnostic produced by a VIR semantic pass.
///
/// All variants carry only structural data — no preformatted strings.
/// Spans pinpoint exact source locations; IDs reference other structural
/// nodes (items, bindings, types); enums classify kinds (binary operators,
/// capability slots) that previously lived as strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrDiagnostic {
    // -----------------------------------------------------------------
    // Resolve pass
    // -----------------------------------------------------------------
    /// A name reference could not be resolved to any definition.
    Unresolved {
        /// The name reference span — points at the offending identifier.
        name: SourceSpan,
    },
    /// A name resolved but to a kind that isn't valid here
    /// (e.g. assigning to a function, struct literal of a non-struct).
    InvalidTarget {
        /// The reference span (e.g. LHS of an `=`).
        name: SourceSpan,
        /// What we actually found.
        found: DefTarget,
    },

    // -----------------------------------------------------------------
    // Type-check pass
    // -----------------------------------------------------------------
    /// An argument's type doesn't match the parameter's declared type.
    ArgTypeMismatch {
        /// The argument expression's span.
        arg: SourceSpan,
        /// The parameter declaration's name token in the called fn.
        param: SourceSpan,
        /// The parameter's declared type, interned in [`vx_types::TypeEnv`].
        expected: TypeId,
        /// The argument's inferred type, interned in [`vx_types::TypeEnv`].
        found: TypeId,
    },
    /// A binary operator does not apply to the operand types.
    BinaryOpMismatch {
        /// Span of the operator token.
        op: SourceSpan,
        /// Which binary operator.
        op_kind: BinOp,
        /// Span of the LHS expression.
        lhs: SourceSpan,
        /// Span of the RHS expression.
        rhs: SourceSpan,
        /// Type of the LHS operand, interned in [`vx_types::TypeEnv`].
        lhs_ty: TypeId,
        /// Type of the RHS operand, interned in [`vx_types::TypeEnv`].
        rhs_ty: TypeId,
    },
    /// Method lookup failed on the given receiver type.
    NoSuchMethod {
        /// Span of the `.method_name` token (or the method-call callee).
        method_call: SourceSpan,
        /// Span of the receiver expression.
        receiver: SourceSpan,
        /// The receiver's inferred type, interned in [`vx_types::TypeEnv`].
        receiver_ty: TypeId,
        /// Method candidates discovered while searching (may be empty).
        candidates: Vec<MethodCandidate>,
    },
    /// A bare expression statement that doesn't flow into a value.
    UnusedExprStatement {
        /// Span of the offending expression.
        expr: SourceSpan,
    },

    // -----------------------------------------------------------------
    // Cap-check pass
    // -----------------------------------------------------------------
    /// A closure's inferred capabilities exceed the mask allowed by the
    /// callback slot it was passed to.
    CapabilityViolation {
        /// The closure expression that exceeds the slot's allowed capabilities.
        closure: SourceSpan,
        /// The slot declaration's name token in the called fn's signature.
        slot: SourceSpan,
        /// Capabilities the slot allows.
        allowed: CapSet,
        /// Capabilities the closure body actually uses.
        found: CapSet,
    },
    /// An unknown capability tag was used.
    UnknownCapability {
        /// The capability name token in source.
        name: SourceSpan,
    },

    // -----------------------------------------------------------------
    // CLI-literal grammar check (`@grammar[ … ]`)
    // -----------------------------------------------------------------
    /// A CLI literal failed validation against its tool grammar. The span
    /// points at the offending source (the grammar name, or the bad argument);
    /// `kind` carries the grammar-domain detail.
    Cli {
        span: SourceSpan,
        kind: CliDiagnosticKind,
    },
}

/// What a [`IrDiagnostic::Cli`] found wrong checking `@grammar[ … ]` against its
/// grammar. The strings are grammar-domain data (flag names, values, modes),
/// not source identifiers — the source location is carried by the diagnostic's
/// span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliDiagnosticKind {
    /// `@name[ … ]` where `name` resolves to no imported grammar in scope.
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
    /// An interpolation hole whose Vixen type doesn't match the type the grammar
    /// position demands (a path position spliced with a non-path, etc.).
    HoleType {
        /// What the grammar position expects.
        expected: CliHoleExpect,
        /// The hole expression's inferred Vixen type.
        found: TypeId,
    },
}

/// The type a grammar position demands of an interpolation hole. Read off the
/// position's [`command_model::ValueDomain`] / [`command_model::Kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliHoleExpect {
    /// A path position (`ValueDomain::Path`, or an input/output `Kind`): the
    /// hole must be a `Path`/`ExecPath` (or an array of them when repeatable).
    Path,
    /// A textual position (`ValueDomain::OneOf`/`Named`/`Any`, `Kind::Text`):
    /// the hole must be a `String` (or an array of them when repeatable).
    Text,
}

/// A method candidate surfaced as part of a [`IrDiagnostic::NoSuchMethod`]
/// diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodCandidate {
    /// The item ID of the candidate method (its enclosing fn).
    pub item: ItemId,
    /// The name token of the method's declaration.
    pub name_span: SourceSpan,
}

impl IrDiagnostic {
    /// The primary source span associated with this diagnostic — the
    /// location to surface to the user as the "main" cursor for the error.
    pub fn primary_span(&self) -> SourceSpan {
        match self {
            IrDiagnostic::Unresolved { name } => *name,
            IrDiagnostic::InvalidTarget { name, .. } => *name,
            IrDiagnostic::ArgTypeMismatch { arg, .. } => *arg,
            IrDiagnostic::BinaryOpMismatch { op, .. } => *op,
            IrDiagnostic::NoSuchMethod { method_call, .. } => *method_call,
            IrDiagnostic::UnusedExprStatement { expr } => *expr,
            IrDiagnostic::CapabilityViolation { closure, .. } => *closure,
            IrDiagnostic::UnknownCapability { name } => *name,
            IrDiagnostic::Cli { span, .. } => *span,
        }
    }

    /// Secondary source locations that contextualize the error. The LSP
    /// surfaces these as `DiagnosticRelatedInformation`; the terminal
    /// renderer turns them into secondary annotations.
    pub fn related_spans(&self) -> Vec<RelatedSpan> {
        match self {
            IrDiagnostic::Unresolved { .. } => Vec::new(),
            IrDiagnostic::UnusedExprStatement { .. } => Vec::new(),
            IrDiagnostic::UnknownCapability { .. } => Vec::new(),
            IrDiagnostic::InvalidTarget { .. } => Vec::new(),
            IrDiagnostic::ArgTypeMismatch { param, .. } => vec![RelatedSpan {
                span: *param,
                kind: RelatedKind::ParameterDeclaration,
            }],
            IrDiagnostic::BinaryOpMismatch { lhs, rhs, .. } => vec![
                RelatedSpan {
                    span: *lhs,
                    kind: RelatedKind::Operand,
                },
                RelatedSpan {
                    span: *rhs,
                    kind: RelatedKind::Operand,
                },
            ],
            IrDiagnostic::NoSuchMethod {
                receiver,
                candidates,
                ..
            } => {
                let mut out = vec![RelatedSpan {
                    span: *receiver,
                    kind: RelatedKind::Receiver,
                }];
                for c in candidates {
                    out.push(RelatedSpan {
                        span: c.name_span,
                        kind: RelatedKind::Candidate,
                    });
                }
                out
            }
            IrDiagnostic::CapabilityViolation { slot, .. } => vec![RelatedSpan {
                span: *slot,
                kind: RelatedKind::SlotDeclaration,
            }],
            IrDiagnostic::Cli { .. } => Vec::new(),
        }
    }
}

/// A secondary source span attached to a diagnostic, with a kind tag that
/// callers (LSP, renderer) map to a localized label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelatedSpan {
    pub span: SourceSpan,
    pub kind: RelatedKind,
}

/// What KIND of secondary location this is. The LSP / renderer chooses
/// the user-visible wording from this tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelatedKind {
    /// "parameter declared here"
    ParameterDeclaration,
    /// "slot declared here"
    SlotDeclaration,
    /// "receiver of the method call"
    Receiver,
    /// "operand"
    Operand,
    /// "candidate" (member method that might be what the user meant)
    Candidate,
}
