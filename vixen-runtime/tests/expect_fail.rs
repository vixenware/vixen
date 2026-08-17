//! `expect_fail(subject, PayloadType)` — a raise a program declares is an
//! outcome its own checks can assert.
//!
//! Before this, a `fail` reachable from a check was only ever a RED CHECK. A
//! deliberate loud refusal — the interesting half of a decoder or a resolver —
//! could be written and could not be pinned in the same file that defined it,
//! so it was proven out-of-tree by hand or not at all.
//!
//! The constructor names the payload TYPE rather than merely asserting that
//! something went wrong, and that is the whole design. `match subject? { Err(_)
//! => expect(true) }` has been writable since rung 068 and accepts ANY collapse:
//! a typo'd fixture path, a missing map key, a process that died. Those are the
//! machine saying the program is broken, and a check that goes green on them is
//! worse than no check. A machine failure carries no authored payload at all, so
//! it can never match an expected schema.

use vixen_runtime::ratchet::{RatchetReport, run_source, run_source_with_lane};
use weavy::exec::LaneRequest;

const PAYLOAD_DECLS: &str = r#"
struct Refused { why: String }
struct OtherRefusal { why: String }
fn refuse(should: Bool) -> String {
    if should { fail Refused { why: "asked to" } } else { "fine" }
}
fn refuse_otherwise(should: Bool) -> String {
    if should { fail OtherRefusal { why: "asked to" } } else { "fine" }
}
"#;

fn run(test_body: &str) -> RatchetReport {
    let source = format!("{PAYLOAD_DECLS}\n#[test]\nfn t() -> Stream<Check> {{\n{test_body}\n}}\n");
    run_source(&source).expect("an expected raise is a check outcome, never a machine crash")
}

#[test]
fn a_raise_of_the_named_payload_passes() {
    let report = run("    yield expect_fail(refuse(true), Refused);");
    assert!(report.passed(), "{:#?}", report.plain.checks);
    assert!(
        report.agrees(),
        "plain and chaos agree on the expected raise"
    );
    // A green check reports no failure: the collapse WAS the assertion, and
    // leaving it attached would make this run read as red to every reader that
    // treats a present failure as one.
    assert!(
        report.plain.checks[0].failure.is_none(),
        "the satisfied expectation is not also reported as a failure: {:#?}",
        report.plain.checks[0]
    );
}

#[test]
fn a_subject_that_publishes_is_red() {
    // The vacuity that matters most: a check that passes whether or not the
    // program still refuses is not a check.
    let report = run("    yield expect_fail(refuse(false), Refused);");
    assert!(!report.passed(), "a subject that published is red");
    assert!(
        report.plain.checks[0].failure.is_none(),
        "nothing failed — the red verdict is the whole report"
    );
}

#[test]
fn a_raise_of_a_different_payload_is_red() {
    // Two refusals in one function are two different claims. Naming one and
    // getting the other is exactly the regression this constructor exists to
    // catch, so it cannot be waved through as "it failed, close enough".
    let report = run("    yield expect_fail(refuse_otherwise(true), Refused);");
    assert!(!report.passed(), "the wrong payload type is red");
    assert!(
        report.plain.checks[0].failure.is_some(),
        "the red check reports the raise it actually got"
    );
}

#[test]
fn a_machine_failure_never_satisfies_an_expected_raise() {
    // The typo case. A missing key is the machine reporting a broken program,
    // not the program declaring a refusal — it carries no authored payload, so
    // there is no schema for it to match and no way for it to go green.
    let report = run(r#"    let empty: Map<String, Int> = %{};
    yield expect_fail(empty.get("missing"), Refused);"#);
    assert!(
        !report.passed(),
        "a machine failure is not an authored raise"
    );
    assert!(
        report.plain.checks[0].failure.is_some(),
        "the machine failure is reported as what went wrong"
    );
}

#[test]
fn the_verdict_is_identical_across_the_native_and_interpreter_lanes() {
    // The subject is an ordinary demand, so the two execution lanes must reach
    // the same collapse and the harness must read the same verdict off it. A
    // check whose truth depended on which lane ran it would be a differential
    // bug that `cross_lane_differential` could only find by accident.
    let source = format!(
        "{PAYLOAD_DECLS}\n#[test]\nfn t() -> Stream<Check> {{\n    yield expect_fail(refuse(true), Refused);\n    yield expect_fail(refuse_otherwise(true), OtherRefusal);\n}}\n"
    );
    let native = run_source_with_lane(&source, LaneRequest::Native).expect("native lane runs");
    let interpreter =
        run_source_with_lane(&source, LaneRequest::Interpreter).expect("interpreter lane runs");
    assert!(native.passed(), "{:#?}", native.plain.checks);
    assert!(interpreter.passed(), "{:#?}", interpreter.plain.checks);
    assert_eq!(
        native.plain.checks, interpreter.plain.checks,
        "the two lanes report the same checks, field for field"
    );
}

#[test]
fn the_expected_payload_type_must_be_a_declared_type() {
    let source = format!(
        "{PAYLOAD_DECLS}\n#[test]\nfn t() -> Stream<Check> {{\n    yield expect_fail(refuse(true), NoSuchPayload);\n}}\n"
    );
    let error = run_source(&source).expect_err("an undeclared payload type is a compile error");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("NoSuchPayload"),
        "the diagnostic names the type that does not exist: {rendered}"
    );
}

#[test]
fn the_expected_payload_is_a_type_not_a_value() {
    // `expect_fail(x, Refused { why: "asked to" })` is a tempting spelling and a
    // different feature: it would demand the payload and compare identities.
    // Refuse it rather than silently accepting the constructor call's type.
    let source = format!(
        "{PAYLOAD_DECLS}\n#[test]\nfn t() -> Stream<Check> {{\n    yield expect_fail(refuse(true), Refused {{ why: \"asked to\" }});\n}}\n"
    );
    let error = run_source(&source).expect_err("a payload VALUE is refused");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("names a payload type"),
        "the refusal says what the argument is for: {rendered}"
    );
}
