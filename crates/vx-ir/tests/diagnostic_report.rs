//! Structural tests for [`IrDiagnostic::to_report`].
//!
//! Each test builds an `IrDiagnostic` by hand with synthetic spans and a
//! minimal `TypeEnv`, calls `to_report`, and asserts on report fields
//! (severity, title, annotation counts, primary span coordinates, etc.).
//!
//! These are *structural* tests — they intentionally do not snapshot the
//! rendered output, since rendering is the next milestone.

use margin::{AnnotationRole, NoteKind, Severity, Span as MarginSpan};
use rowan::{TextRange, TextSize};

use vx_ir::cap::CapSet;
use vx_ir::diagnostic::{IrDiagnostic, MethodCandidate};
use vx_ir::expr::BinOp;
use vx_ir::scope::DefTarget;
use vx_ir::{BindingId, ItemId};
use vx_parser::{SourceId, SourceSpan};
use vx_types::{Scalar, TypeEnv};

fn span(start: u32, end: u32) -> SourceSpan {
    SourceSpan::new(
        SourceId(0),
        TextRange::new(TextSize::new(start), TextSize::new(end)),
    )
}

fn margin_span(start: usize, end: usize) -> MarginSpan {
    MarginSpan {
        source_id: margin::SourceId("0".to_string()),
        start,
        end,
    }
}

// ---------------------------------------------------------------------------
// Unresolved
// ---------------------------------------------------------------------------

#[test]
fn unresolved_basic() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::Unresolved { name: span(10, 13) };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "unresolved name");
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(10, 13)]);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("not found in scope")
    );
    assert!(report.notes.is_empty());
    assert!(report.sections.is_empty());
}

#[test]
fn unresolved_different_span() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::Unresolved { name: span(42, 50) };

    let report = diag.to_report(&env);

    assert_eq!(report.annotations[0].spans, vec![margin_span(42, 50)]);
}

// ---------------------------------------------------------------------------
// InvalidTarget
// ---------------------------------------------------------------------------

#[test]
fn invalid_target_item() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::InvalidTarget {
        name: span(0, 5),
        found: DefTarget::Item(ItemId(7)),
    };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "invalid target");
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("this name refers to a function but a value is expected")
    );
}

#[test]
fn invalid_target_binding() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::InvalidTarget {
        name: span(0, 5),
        found: DefTarget::Binding(BindingId(3)),
    };

    let report = diag.to_report(&env);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("this is a binding, not a function")
    );
}

#[test]
fn invalid_target_variant() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::InvalidTarget {
        name: span(0, 5),
        found: DefTarget::Variant {
            enum_item: ItemId(2),
            variant_index: 0,
        },
    };

    let report = diag.to_report(&env);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("variant cannot be used here")
    );
}

// ---------------------------------------------------------------------------
// ArgTypeMismatch
// ---------------------------------------------------------------------------

#[test]
fn arg_type_mismatch_basic() {
    let mut env = TypeEnv::new();
    let i64_ty = env.intern_scalar(Scalar::I64);
    let string_ty = env.intern_scalar(Scalar::String);
    let diag = IrDiagnostic::ArgTypeMismatch {
        arg: span(10, 12),
        param: span(50, 55),
        expected: i64_ty,
        found: string_ty,
    };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "argument type mismatch");
    assert_eq!(report.annotations.len(), 2);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(10, 12)]);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("expected `I64`, found `String`")
    );
    assert_eq!(report.annotations[1].role, AnnotationRole::SecondaryLabel);
    assert_eq!(report.annotations[1].spans, vec![margin_span(50, 55)]);
    assert_eq!(
        report.annotations[1].message.as_deref(),
        Some("parameter declared here")
    );
}

#[test]
fn arg_type_mismatch_unit() {
    let mut env = TypeEnv::new();
    let unit_ty = env.intern_unit();
    let i64_ty = env.intern_scalar(Scalar::I64);
    let diag = IrDiagnostic::ArgTypeMismatch {
        arg: span(0, 4),
        param: span(20, 24),
        expected: unit_ty,
        found: i64_ty,
    };

    let report = diag.to_report(&env);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("expected `()`, found `I64`")
    );
}

// ---------------------------------------------------------------------------
// BinaryOpMismatch
// ---------------------------------------------------------------------------

#[test]
fn binary_op_mismatch_add() {
    let mut env = TypeEnv::new();
    let i64_ty = env.intern_scalar(Scalar::I64);
    let string_ty = env.intern_scalar(Scalar::String);
    let diag = IrDiagnostic::BinaryOpMismatch {
        op: span(10, 11),
        op_kind: BinOp::Add,
        lhs: span(5, 9),
        rhs: span(12, 16),
        lhs_ty: i64_ty,
        rhs_ty: string_ty,
    };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "binary operator type mismatch");
    assert_eq!(report.annotations.len(), 3);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(10, 11)]);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("`+` cannot be applied to `I64` and `String`")
    );
    // lhs context
    assert_eq!(report.annotations[1].role, AnnotationRole::SecondaryLabel);
    assert_eq!(report.annotations[1].spans, vec![margin_span(5, 9)]);
    assert_eq!(report.annotations[1].message.as_deref(), Some("I64"));
    // rhs context
    assert_eq!(report.annotations[2].role, AnnotationRole::SecondaryLabel);
    assert_eq!(report.annotations[2].spans, vec![margin_span(12, 16)]);
    assert_eq!(report.annotations[2].message.as_deref(), Some("String"));
}

#[test]
fn binary_op_mismatch_pipe() {
    let mut env = TypeEnv::new();
    let bool_ty = env.intern_scalar(Scalar::Bool);
    let diag = IrDiagnostic::BinaryOpMismatch {
        op: span(0, 2),
        op_kind: BinOp::Pipe,
        lhs: span(3, 6),
        rhs: span(7, 10),
        lhs_ty: bool_ty,
        rhs_ty: bool_ty,
    };

    let report = diag.to_report(&env);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("`|>` cannot be applied to `Bool` and `Bool`")
    );
}

// ---------------------------------------------------------------------------
// NoSuchMethod
// ---------------------------------------------------------------------------

#[test]
fn no_such_method_no_candidates() {
    let mut env = TypeEnv::new();
    let i64_ty = env.intern_scalar(Scalar::I64);
    let diag = IrDiagnostic::NoSuchMethod {
        method_call: span(10, 20),
        receiver: span(0, 9),
        receiver_ty: i64_ty,
        candidates: Vec::new(),
    };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "no such method");
    assert_eq!(report.annotations.len(), 2);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("type `I64` has no method here")
    );
    assert_eq!(report.annotations[1].role, AnnotationRole::SecondaryLabel);
    assert_eq!(report.annotations[1].message.as_deref(), Some("I64"));
}

#[test]
fn no_such_method_with_candidates() {
    let mut env = TypeEnv::new();
    let string_ty = env.intern_scalar(Scalar::String);
    let diag = IrDiagnostic::NoSuchMethod {
        method_call: span(10, 20),
        receiver: span(0, 9),
        receiver_ty: string_ty,
        candidates: vec![
            MethodCandidate {
                item: ItemId(1),
                name_span: span(100, 105),
            },
            MethodCandidate {
                item: ItemId(2),
                name_span: span(200, 205),
            },
        ],
    };

    let report = diag.to_report(&env);
    // 2 base + 2 candidates
    assert_eq!(report.annotations.len(), 4);
    assert_eq!(report.annotations[2].role, AnnotationRole::RelatedLabel);
    assert_eq!(report.annotations[2].spans, vec![margin_span(100, 105)]);
    assert_eq!(report.annotations[2].message.as_deref(), Some("candidate"));
    assert_eq!(report.annotations[3].role, AnnotationRole::RelatedLabel);
    assert_eq!(report.annotations[3].spans, vec![margin_span(200, 205)]);
    assert_eq!(report.annotations[3].message.as_deref(), Some("candidate"));
}

// ---------------------------------------------------------------------------
// UnusedExprStatement
// ---------------------------------------------------------------------------

#[test]
fn unused_expr_statement_basic() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::UnusedExprStatement { expr: span(0, 10) };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "unused expression statement");
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(0, 10)]);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("this expression's value is discarded")
    );
}

// ---------------------------------------------------------------------------
// CapabilityViolation
// ---------------------------------------------------------------------------

#[test]
fn capability_violation_basic() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::CapabilityViolation {
        closure: span(0, 20),
        slot: span(50, 60),
        allowed: CapSet::FS,
        found: CapSet::FS.union(CapSet::HOST),
    };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "capability violation");
    assert_eq!(report.annotations.len(), 2);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(0, 20)]);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("inferred capabilities `{fs, host}` exceed allowed `{fs}`")
    );
    assert_eq!(report.annotations[1].role, AnnotationRole::SecondaryLabel);
    assert_eq!(report.annotations[1].spans, vec![margin_span(50, 60)]);
    assert_eq!(
        report.annotations[1].message.as_deref(),
        Some("slot declared with these allowed capabilities here")
    );
    // Help note is attached
    assert_eq!(report.notes.len(), 1);
    assert_eq!(report.notes[0].kind, NoteKind::Help);
}

#[test]
fn capability_violation_empty_allowed() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::CapabilityViolation {
        closure: span(0, 5),
        slot: span(10, 15),
        allowed: CapSet::EMPTY,
        found: CapSet::DISCOVER,
    };

    let report = diag.to_report(&env);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("inferred capabilities `{discover}` exceed allowed `{}`")
    );
}

// ---------------------------------------------------------------------------
// UnknownCapability
// ---------------------------------------------------------------------------

#[test]
fn unknown_capability_basic() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::UnknownCapability { name: span(5, 15) };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "unknown capability");
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(5, 15)]);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("not a recognized capability")
    );
}

// ---------------------------------------------------------------------------
// NonExhaustiveMatch
// ---------------------------------------------------------------------------

#[test]
fn non_exhaustive_match_lists_missing_cases() {
    let env = TypeEnv::new();
    let diag = IrDiagnostic::NonExhaustiveMatch {
        match_span: span(3, 12),
        missing: vec!["Err".to_string()],
    };

    let report = diag.to_report(&env);

    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "non-exhaustive match");
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(3, 12)]);
    assert_eq!(
        report.annotations[0].message.as_deref(),
        Some("missing case(s): Err")
    );
    // A help note suggests adding the arm or a wildcard.
    assert!(report.notes.iter().any(|n| n.text.contains("Err")));
}

// ---------------------------------------------------------------------------
// Source ID mapping
// ---------------------------------------------------------------------------

#[test]
fn non_zero_source_id_is_stringified() {
    let env = TypeEnv::new();
    let custom_span = SourceSpan::new(SourceId(42), TextRange::new(0.into(), 4.into()));
    let diag = IrDiagnostic::Unresolved { name: custom_span };

    let report = diag.to_report(&env);

    assert_eq!(
        report.annotations[0].spans[0].source_id,
        margin::SourceId("42".to_string())
    );
}
