//! Tests for the VIR capability checking pass.

use vx_ir::CapErrorKind;
use vx_ir::build::VirBuilder;
use vx_ir::cap_check::check_capabilities_vir;
use vx_ir::resolve::resolve_names;
use vx_ir::type_check::type_check_vir;
use vx_ir::type_collect::collect_types_vir;
use vx_parser::{SourceId, parse_source_file};

/// Helper: build VIR, resolve names, collect types, type check, cap check.
fn cap_check_source(source: &str) -> Vec<CapErrorKind> {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let tc = collect_types_vir(&mut program.arena);
    let _resolve_errors = resolve_names(&mut program.arena);
    let tc_result = type_check_vir(&mut program.arena, &tc);
    let cap_result = check_capabilities_vir(&program.arena, &tc, &tc_result);
    cap_result.errors.into_iter().map(|e| e.kind).collect()
}

// =========================================================================
// 1. Unknown capability tag
// =========================================================================

#[test]
fn unknown_cap_tag() {
    let errors = cap_check_source("fn f() #bogus ( 1 )");
    assert_eq!(errors.len(), 1);
    assert!(
        matches!(&errors[0], CapErrorKind::UnknownCapability { name } if name == "bogus"),
        "expected UnknownCapability, got {:?}",
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
        CapErrorKind::UnknownCapability { name } if name == "nope"
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
        CapErrorKind::UnknownCapability { name } if name == "invalid"
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
        CapErrorKind::UnknownCapability { name } if name == "bogus2"
    ));
}
