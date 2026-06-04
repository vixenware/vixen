//! Integration tests for [`vx_diagnostics::render_compile_error`].
//!
//! These exercise the structural conversion from
//! [`vx_ir::compile::CompileError`] into [`margin::Diagnostics`]: number of
//! reports, severities, source collection (user input + any referenced
//! stdlib source). We deliberately avoid snapshotting ANSI output here —
//! that belongs in margin-term coverage.

use vx_diagnostics::render_compile_error;
use vx_parser::SourceId;
use vx_runtime::compile::compile_source;
use vx_types::TypeEnv;

#[test]
fn render_type_error_produces_report_with_user_and_stdlib_sources() {
    // `1 + true` is a type error against the binary `+` operator. Diagnostics
    // reference both the user source and a stdlib source (the operator's
    // declaration site for "context").
    let source = r#"
fn probe() -> u64 (
  1 + true
)
"#;

    let err = compile_source(source, "probe").expect_err("expected a compile error");
    let fallback_env = TypeEnv::default();
    let diags = render_compile_error(&err, source, SourceId(0), Some("user.vx"), &fallback_env);

    assert!(
        !diags.reports.is_empty(),
        "expected at least one report, got 0"
    );
    for report in &diags.reports {
        assert_eq!(report.severity, margin::Severity::Error);
        assert!(!report.title.is_empty(), "report title should not be empty");
    }

    // The user source must always be present in the source list.
    let has_user = diags
        .sources
        .iter()
        .any(|s| s.id == margin::SourceId("0".to_string()) && s.name == "user.vx");
    assert!(has_user, "expected user.vx source in diagnostics.sources");

    // Sanity: every source id referenced by an annotation should have a
    // corresponding entry in diagnostics.sources.
    for report in &diags.reports {
        for annotation in &report.annotations {
            for span in &annotation.spans {
                assert!(
                    diags.sources.iter().any(|s| s.id == span.source_id),
                    "annotation references missing source {:?}",
                    span.source_id
                );
            }
        }
    }
}

#[test]
fn render_parse_error_produces_report_referencing_user_source() {
    // Missing closing paren after function body.
    let source = "fn broken() -> u64 ( 1\n";

    let err = compile_source(source, "broken").expect_err("expected parse error");
    let fallback_env = TypeEnv::default();
    let diags = render_compile_error(&err, source, SourceId(0), Some("user.vx"), &fallback_env);

    assert!(!diags.reports.is_empty());
    let has_user = diags
        .sources
        .iter()
        .any(|s| s.id == margin::SourceId("0".to_string()));
    assert!(has_user, "expected user source to be present");
}

#[test]
fn render_missing_entry_produces_report_with_note() {
    // Source compiles successfully but the entry name we ask for doesn't
    // exist — exercises CompileError::MissingEntry.
    let source = "fn other() -> U64 ( 1 )\n";

    let err = compile_source(source, "missing_one").expect_err("expected missing entry");
    let fallback_env = TypeEnv::default();
    let diags = render_compile_error(&err, source, SourceId(0), Some("user.vx"), &fallback_env);

    assert_eq!(diags.reports.len(), 1);
    let report = &diags.reports[0];
    assert_eq!(report.severity, margin::Severity::Error);
    assert!(
        report.notes.iter().any(|n| n.text.contains("missing_one")),
        "expected note mentioning the entry name, got {:?}",
        report.notes
    );
    // No source spans for this variant — but the user source is still
    // populated so clients have something to anchor on.
    assert!(diags.sources.iter().any(|s| s.name == "user.vx"));
}
