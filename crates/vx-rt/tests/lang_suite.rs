//! Self-hosting test harness.
//!
//! Vixen tests live in `tests/lang/*.vx`, written in Vixen. This one generic
//! Rust runner discovers every `#[test]` function across those files and runs
//! it through the evaluator. A test passes iff it evaluates **without
//! poisoning** — clean evaluation = pass; any poison (a `fail`, a forced `💣`,
//! a runtime error) = fail. The detonation site is the report.
//!
//! `#[test(poisons)]` inverts it: the body is *expected* to poison, so
//! detonation = pass (the error-path form). This is a smoke harness for the
//! dev loop; the standalone `.vx` runner (CLI + JSON/HTML reports for CI) is a
//! separate tool.

use std::fs;
use std::path::{Path, PathBuf};

use vx_parser::{Item, SourceId, parse_source_file};
use vx_rt::{RtValue, eval_source};

/// Prepended to every `.vx` file so assertions are themselves Vixen. A test
/// that wants a different prelude does not exist yet; when one does, this moves
/// into a shared `tests/lang/_prelude.vx` discovered alongside the cases.
const PRELUDE: &str = r#"
fn assert(cond: Bool) -> Bool (
    if cond ( true ) else ( fail "assertion failed" )
)

fn assert_eq[T](got: T, want: T) -> Bool (
    if got == want ( true ) else ( fail "not equal" )
)
"#;

#[derive(Clone, Copy, Debug)]
enum Expect {
    /// `#[test]` — must evaluate without poisoning.
    Clean,
    /// `#[test(poisons)]` — must poison (error-path test).
    Poisons,
}

struct LangTest {
    file: PathBuf,
    fn_name: String,
    expect: Expect,
}

fn lang_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lang")
}

/// Read every `.vx` file and collect its annotated test functions.
fn discover() -> Vec<LangTest> {
    let dir = lang_dir();
    let mut tests = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "vx"))
        .collect();
    entries.sort();

    for file in entries {
        let source = fs::read_to_string(&file).expect("read .vx");
        let parsed = parse_source_file(&source, SourceId(0));
        assert!(
            parsed.errors().is_empty(),
            "{} has parse errors: {:?}",
            file.display(),
            parsed.errors()
        );
        for item in parsed.tree().items() {
            let Item::FnDecl(func) = item else { continue };
            let Some(expect) = test_kind(&func) else {
                continue;
            };
            let fn_name = func
                .name()
                .map(|t| t.text().to_string())
                .expect("test fn has a name");
            tests.push(LangTest {
                file: file.clone(),
                fn_name,
                expect,
            });
        }
    }
    tests
}

/// Classify a function by its attributes: `#[test]` → clean test,
/// `#[test(poisons)]` → error-path test.
fn test_kind(func: &vx_parser::FnDecl) -> Option<Expect> {
    let mut is_test = false;
    let mut poisons = false;
    if let Some(list) = func.attributes() {
        for attr in list.attributes() {
            for entry in attr.entries() {
                if entry.key().map(|t| t.text().to_string()).as_deref() == Some("test") {
                    is_test = true;
                    for arg in entry.args() {
                        if arg.key().map(|t| t.text().to_string()).as_deref() == Some("poisons") {
                            poisons = true;
                        }
                    }
                }
            }
        }
    }
    if !is_test {
        return None;
    }
    Some(if poisons {
        Expect::Poisons
    } else {
        Expect::Clean
    })
}

/// True if the evaluation outcome is a poison: a forced `💣` / `fail` surfaces
/// as `Ok(RtValue::Failure)`, a runtime/compile error as `Err`.
fn poisoned(result: &Result<RtValue, vx_rt::RtError>) -> bool {
    matches!(result, Err(_) | Ok(RtValue::Failure(_)))
}

#[test]
fn lang_suite() {
    let tests = discover();
    assert!(
        !tests.is_empty(),
        "no .vx tests discovered in {}",
        lang_dir().display()
    );

    let mut failures = Vec::new();
    for t in &tests {
        let source = fs::read_to_string(&t.file).expect("read .vx");
        let full = format!("{PRELUDE}\n{source}");
        let result = eval_source(&full, &t.fn_name, &[]);
        let case = format!(
            "{}::{}",
            t.file.file_name().unwrap().to_string_lossy(),
            t.fn_name
        );
        match t.expect {
            Expect::Clean if poisoned(&result) => {
                failures.push(format!("{case} — expected clean, got: {result:?}"));
            }
            Expect::Poisons if !poisoned(&result) => {
                failures.push(format!("{case} — expected poison, got: {result:?}"));
            }
            _ => {}
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} .vx test(s) failed:\n{}",
        failures.len(),
        tests.len(),
        failures.join("\n")
    );
}
