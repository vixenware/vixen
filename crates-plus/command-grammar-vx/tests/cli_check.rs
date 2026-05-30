//! The CLI-literal checker, end-to-end with zero evaluation: parse a `@cc[ … ]`
//! literal, load `cc.vx`, and check the literal against the grammar — unknown
//! flags, values outside a fixed domain, and the typed expectation of each
//! interpolation hole.

use command_grammar_vx::check::{CliDiagnostic, check_cli_literal};
use command_grammar_vx::{load_str, parse};
use command_model::{Kind, ValueDomain};
use vx_parser::{AstNode, CliLiteralExpr};

const CC_VX: &str = include_str!("../../command-model/grammars/cc.vx");

/// Parse a single CLI literal out of a `let` binding.
fn literal(src: &str) -> (vx_parser::Parsed<vx_parser::SourceFile>, CliLiteralExpr) {
    let parsed = parse(src);
    assert_eq!(parsed.errors(), [], "the snippet should parse");
    let lit = parsed
        .syntax()
        .descendants()
        .find_map(CliLiteralExpr::cast)
        .expect("a CLI literal");
    (parsed, lit)
}

fn check(src: &str) -> command_grammar_vx::check::Checked {
    let set = load_str(CC_VX).expect("cc.vx loads");
    let (_keep, lit) = literal(src);
    check_cli_literal(&lit, &set)
}

#[test]
fn clean_literal_has_no_diagnostics() {
    let checked = check("let o = @cc[ -c -O2 -I src main.c -o out.o ]\n");
    assert!(
        checked.diagnostics.is_empty(),
        "a valid command checks clean; got {:?}",
        checked.diagnostics
    );
}

#[test]
fn unknown_flag_is_reported() {
    let checked = check("let o = @cc[ -c -frobnicate main.c ]\n");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(d, CliDiagnostic::UnknownFlag { token } if token == "-frobnicate")),
        "got {:?}",
        checked.diagnostics
    );
}

#[test]
fn value_outside_the_domain_is_reported() {
    // cc's -std is OneOf [c99, c11, c17, c23]; c++99 is not allowed.
    let checked = check("let o = @cc[ -std=c++99 main.c ]\n");
    assert!(
        checked.diagnostics.iter().any(|d| matches!(
            d,
            CliDiagnostic::ValueNotAllowed { flag, value, .. } if flag == "-std" && value == "c++99"
        )),
        "got {:?}",
        checked.diagnostics
    );
}

#[test]
fn unknown_grammar_is_reported() {
    let set = load_str(CC_VX).unwrap();
    let (_keep, lit) = literal("let o = @nosuch[ -x ]\n");
    let checked = check_cli_literal(&lit, &set);
    assert_eq!(
        checked.diagnostics,
        vec![CliDiagnostic::UnknownGrammar {
            name: "nosuch".to_string()
        }]
    );
}

#[test]
fn holes_carry_the_grammar_expectation() {
    // `-o $out` (a separated output path) and `-I $dir` (an include dir) — each
    // hole's expected role/kind/value-domain is read off its grammar position.
    let checked = check("let o = @cc[ -c -I $dir main.c -o $out ]\n");
    assert!(
        checked.diagnostics.is_empty(),
        "no static errors; got {:?}",
        checked.diagnostics
    );

    let include = checked
        .holes
        .iter()
        .find(|h| h.source == "$dir")
        .expect("a hole for $dir");
    assert_eq!(include.role, "include");
    assert_eq!(include.kind, Kind::Input { dir: true });
    assert_eq!(include.value, ValueDomain::Path { dir: true });

    let output = checked
        .holes
        .iter()
        .find(|h| h.source == "$out")
        .expect("a hole for $out");
    assert_eq!(output.role, "output");
    assert_eq!(output.kind, Kind::Output { dir: false });
    assert_eq!(output.value, ValueDomain::Path { dir: false });
}

#[test]
fn long_form_hole_carries_its_expression() {
    // `${ … }` splices a full expression; the expectation records its source.
    let checked = check("let o = @cc[ -c main.c -o ${ out_dir.join(\"x.o\") } ]\n");
    let output = checked
        .holes
        .iter()
        .find(|h| h.role == "output")
        .expect("an output hole");
    assert_eq!(output.source, "out_dir.join(\"x.o\")");
    assert_eq!(output.value, ValueDomain::Path { dir: false });
}
