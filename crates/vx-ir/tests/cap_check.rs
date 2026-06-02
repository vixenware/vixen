//! Tests for the VIR capability checking pass.

use vx_ir::IrDiagnostic;
use vx_ir::build::VirBuilder;
use vx_ir::cap_check::check_capabilities_vir;
use vx_ir::resolve::resolve_names;
use vx_ir::type_check::type_check_vir;
use vx_ir::type_collect::collect_types_vir;
use vx_parser::{SourceId, parse_source_file};

/// Helper: build VIR, resolve names, collect types, type check, cap check.
/// Returns the resulting diagnostics paired with the source text of the
/// primary span — tests assert against name shape, not raw strings inside
/// the diagnostic (which no longer exist).
fn cap_check_source(source: &str) -> Vec<(IrDiagnostic, String)> {
    let parsed = parse_source_file(source, SourceId(0));
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let mut tc = collect_types_vir(&mut program.arena);
    let _resolve_errors = resolve_names(&mut program.arena);
    let tc_result = type_check_vir(&mut program.arena, &mut tc);
    let _eu_errors =
        vx_ir::error_union::infer_error_unions(&mut program.arena, &mut tc, &tc_result);
    let cap_result = check_capabilities_vir(&program.arena, &tc, &tc_result);
    cap_result
        .errors
        .into_iter()
        .map(|e| {
            let span = e.primary_span();
            let start = span.start() as usize;
            let end = span.end() as usize;
            let text = source.get(start..end).unwrap_or("").to_string();
            (e, text)
        })
        .collect()
}

// =========================================================================
// 1. Unknown capability tag
// =========================================================================

#[test]
fn unknown_cap_tag() {
    let errors = cap_check_source("fn f() #bogus ( 1 )");
    assert_eq!(errors.len(), 1);
    assert!(
        matches!(&errors[0].0, IrDiagnostic::UnknownCapability { .. }) && errors[0].1 == "bogus",
        "expected UnknownCapability for `bogus`, got {:?}",
        errors[0]
    );
}

// =========================================================================
// 2. Known capability tag — no errors
// =========================================================================

#[test]
fn known_cap_tag_no_error() {
    let errors = cap_check_source("fn f() #fs ( 1 )");
    assert!(errors.is_empty(), "expected no errors, got {:?}", errors);
}

// =========================================================================
// 3. Multiple known tags
// =========================================================================

#[test]
fn multiple_known_tags() {
    let errors = cap_check_source("fn f() #fs #discover ( 1 )");
    assert!(errors.is_empty(), "expected no errors, got {:?}", errors);
}

// =========================================================================
// 4. Mixed known and unknown tags
// =========================================================================

#[test]
fn mixed_known_unknown_tags() {
    let errors = cap_check_source("fn f() #fs #nope ( 1 )");
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        &errors[0],
        (IrDiagnostic::UnknownCapability { .. }, text) if text == "nope"
    ));
}

// =========================================================================
// 5. No caps, no errors
// =========================================================================

#[test]
fn no_caps_no_errors() {
    let errors = cap_check_source("fn f() ( 1 )");
    assert!(errors.is_empty());
}

// =========================================================================
// 6. Local fn body caps don't propagate
// =========================================================================

#[test]
fn local_fn_caps_dont_propagate() {
    // A local fn with #fs inside a closure body should not cause the
    // enclosing closure to be flagged as having fs caps.
    let errors = cap_check_source(
        r#"
        fn outer() (
            fn inner() #fs ( 1 );
            inner()
        )
        "#,
    );
    assert!(errors.is_empty(), "expected no errors, got {:?}", errors);
}

// =========================================================================
// 7. Top-level code is checked
// =========================================================================

#[test]
fn top_level_code_checked() {
    // An unknown cap tag on a top-level function should be caught.
    let errors = cap_check_source("fn f() #invalid ( 1 ); f()");
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        &errors[0],
        (IrDiagnostic::UnknownCapability { .. }, text) if text == "invalid"
    ));
}

// =========================================================================
// 8. Namespace function caps collected
// =========================================================================

#[test]
fn namespace_fn_unknown_cap() {
    let errors = cap_check_source(
        r#"
        namespace Foo (
            fn bar() #bogus2 ( 1 )
        )
        "#,
    );
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        &errors[0],
        (IrDiagnostic::UnknownCapability { .. }, text) if text == "bogus2"
    ));
}
