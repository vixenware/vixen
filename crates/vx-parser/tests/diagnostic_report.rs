//! Structural tests for [`ParseDiagnostic::to_report`].
//!
//! Each test builds a `ParseDiagnostic` by hand with synthetic spans,
//! calls `to_report`, and asserts on report fields (severity, title,
//! annotation counts, primary span coordinates, etc.).

use margin::{AnnotationRole, Severity, Span as MarginSpan};
use rowan::{TextRange, TextSize};

use vx_parser::{
    ExpectedKind, LegacySyntaxKind, ParseDiagnostic, SourceId, SourceSpan, SyntaxKind,
};

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

#[test]
fn expected_token_renders_with_token_label() {
    let diag = ParseDiagnostic::Expected {
        what: ExpectedKind::Token(SyntaxKind::SEMICOLON),
        at: span(10, 11),
    };

    let report = diag.to_report();

    assert_eq!(report.severity, Severity::Error);
    assert!(
        report.title.contains('`'),
        "title should include the token label in backticks: {:?}",
        report.title
    );
    assert!(
        report.title.contains(';'),
        "title should mention the semicolon token: {:?}",
        report.title
    );
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(10, 11)]);
}

#[test]
fn expected_token_or_semicolon_mentions_both() {
    let diag = ParseDiagnostic::Expected {
        what: ExpectedKind::TokenOrSemicolon(SyntaxKind::R_CURLY),
        at: span(0, 1),
    };

    let report = diag.to_report();
    let msg = report.annotations[0]
        .message
        .as_deref()
        .expect("primary annotation should have a message");
    assert!(msg.contains('}'), "message should mention `}}`: {msg:?}");
    assert!(msg.contains(';'), "message should mention `;`: {msg:?}");
}

#[test]
fn expected_declaration_has_structural_message() {
    let diag = ParseDiagnostic::Expected {
        what: ExpectedKind::Declaration,
        at: span(3, 7),
    };

    let report = diag.to_report();
    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].spans, vec![margin_span(3, 7)]);
    let msg = report.annotations[0]
        .message
        .as_deref()
        .expect("primary annotation should have a message");
    assert!(
        msg.contains("declaration"),
        "message should mention `declaration`: {msg:?}"
    );
}

#[test]
fn expected_type_name_primary_span_matches_diagnostic_span() {
    let diag = ParseDiagnostic::Expected {
        what: ExpectedKind::TypeName,
        at: span(42, 50),
    };

    let report = diag.to_report();
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].spans, vec![margin_span(42, 50)]);
}

#[test]
fn legacy_at_attribute_renders_as_deprecated_syntax() {
    let diag = ParseDiagnostic::LegacySyntax {
        kind: LegacySyntaxKind::AtAttribute,
        at: span(0, 8),
    };

    let report = diag.to_report();
    assert_eq!(report.severity, Severity::Error);
    assert_eq!(report.title, "deprecated syntax");
    assert_eq!(report.annotations.len(), 1);
    assert_eq!(report.annotations[0].role, AnnotationRole::PrimaryLabel);
    assert_eq!(report.annotations[0].spans, vec![margin_span(0, 8)]);
    let msg = report.annotations[0]
        .message
        .as_deref()
        .expect("primary annotation should have a message");
    assert!(
        msg.contains("#[") || msg.contains("attribute"),
        "message should explain the replacement: {msg:?}"
    );
}

#[test]
fn primary_span_helper_returns_diagnostic_span() {
    let diag = ParseDiagnostic::Expected {
        what: ExpectedKind::Identifier,
        at: span(13, 18),
    };
    assert_eq!(diag.primary_span(), span(13, 18));

    let legacy = ParseDiagnostic::LegacySyntax {
        kind: LegacySyntaxKind::AtAttribute,
        at: span(20, 25),
    };
    assert_eq!(legacy.primary_span(), span(20, 25));
}
