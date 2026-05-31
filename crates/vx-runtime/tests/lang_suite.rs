//! Self-hosting `.vx` test harness for the demand-driven runtime.
//!
//! The tests live in `tests/lang/*.vx`, written in Vixen. This one generic Rust
//! runner discovers every annotated function and runs each through `eval` — the
//! tests are `.vx`, the runner is the only Rust.
//!
//! Each test is run in **every available [`KernelMode`]** — pure tree-walk
//! (`Off`), the kernel interpreter (`Interp`), and on native the JIT (`Jit`) —
//! and the outcomes must be **identical across all of them**. That cross-mode
//! parity is the oracle: `Off` is the ground truth the kernel is checked against,
//! and the interpreter is the ground truth the JIT is checked against, applied to
//! every test for free.
//!
//! Annotations (read by the harness; inert metadata to the language):
//! - `#[test]` — must evaluate **clean** (no poison) in every mode.
//! - `#[test(poisons)]` — must **poison** in every mode (the error-path form: a
//!   `fail`, a forced `💣`, a failed `assert`).
//! - `#[test(kernel)]` — additionally, the body must dispatch ≥1 kernel (observed
//!   in `Interp` mode), proving it took the kernel path rather than tree-walking.
//!
//! `assert`/`assert_eq` come from the stdlib prelude; `💣` is the over-forcing
//! tripwire. The detonation site (the poison reason) is the report.

use std::fs;
use std::path::{Path, PathBuf};

use vx_parser::{Item, SourceId, parse_source_file};
use vx_runtime::eval::{KernelMode, Outcome, Value, run_in_mode};

#[derive(Clone, Copy, Debug)]
struct Expect {
    /// `#[test(poisons)]` — the body is expected to detonate.
    poisons: bool,
    /// `#[test(kernel)]` — the body must dispatch at least one kernel.
    kernel: bool,
}

struct LangTest {
    file: PathBuf,
    fn_name: String,
    expect: Expect,
    /// The whole `.vx` file — `run_in_mode` builds it and selects `fn_name`.
    source: String,
}

/// One mode's evaluation result: the mode, and either `(outcome, dispatch count)`
/// or a setup error (parse / entry-not-found).
type ModeRun = (KernelMode, Result<(Outcome, u64), String>);

fn lang_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lang")
}

/// The modes to run each test in. `Off` and `Interp` everywhere; `Jit` only where
/// the JIT exists — on other targets `Jit` would merely re-run the interpreter.
fn modes() -> Vec<KernelMode> {
    let mut m = vec![KernelMode::Off, KernelMode::Interp];
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    m.push(KernelMode::Jit);
    m
}

/// Read every `.vx` file under `tests/lang/` and collect its annotated functions.
fn discover() -> Vec<LangTest> {
    let dir = lang_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "vx"))
        .collect();
    entries.sort();

    let mut tests = Vec::new();
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
                source: source.clone(),
            });
        }
    }
    tests
}

/// Classify a function by its attributes: `#[test]` is a test; `poisons` / `kernel`
/// are flags the harness acts on. The language never sees these — they are inert
/// metadata, exactly as inert as an unrecognized attribute.
fn test_kind(func: &vx_parser::FnDecl) -> Option<Expect> {
    let mut is_test = false;
    let mut poisons = false;
    let mut kernel = false;
    if let Some(list) = func.attributes() {
        for attr in list.attributes() {
            for entry in attr.entries() {
                if entry.key().map(|t| t.text().to_string()).as_deref() != Some("test") {
                    continue;
                }
                is_test = true;
                for arg in entry.args() {
                    match arg.key().map(|t| t.text().to_string()).as_deref() {
                        Some("poisons") => poisons = true,
                        Some("kernel") => kernel = true,
                        _ => {}
                    }
                }
            }
        }
    }
    is_test.then_some(Expect { poisons, kernel })
}

/// A canonical key for an outcome, for cross-mode comparison. A clean scalar
/// compares by value; **any** poison compares equal to any poison (the reason and
/// span legitimately differ between the tree-walker and the kernel — what parity
/// asserts is "same value, or both poison", not identical error text). A
/// non-scalar clean result is rejected: tests should end in `assert_eq` (a Bool),
/// not return a raw compound, so parity stays decidable.
fn outcome_key(o: &Outcome) -> Result<String, String> {
    match o {
        Outcome::Poisoned { .. } => Ok("poison".into()),
        Outcome::Clean(v) => match v {
            Value::U64(n) => Ok(format!("u64:{n}")),
            Value::Bool(b) => Ok(format!("bool:{b}")),
            Value::Str(s) => Ok(format!("str:{s}")),
            Value::Unit => Ok("unit".into()),
            other => Err(format!(
                "non-scalar result {other:?} — end the test in `assert_eq` (a Bool)"
            )),
        },
    }
}

/// The poison reason, for failure reporting.
fn reason_of(o: &Outcome) -> &str {
    match o {
        Outcome::Poisoned { reason, .. } => reason,
        Outcome::Clean(_) => "<clean>",
    }
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
        let case = format!(
            "{}::{}",
            t.file.file_name().unwrap().to_string_lossy(),
            t.fn_name
        );

        // Run in every available mode.
        let runs: Vec<ModeRun> = modes()
            .into_iter()
            .map(|m| (m, run_in_mode(&t.source, &t.fn_name, m)))
            .collect();

        // Setup failures (parse / entry-not-found) are hard fails.
        if let Some((m, Err(e))) = runs.iter().find(|(_, r)| r.is_err()) {
            failures.push(format!("{case} [{m:?}] — setup error: {e}"));
            continue;
        }
        let outs: Vec<(KernelMode, Outcome, u64)> = runs
            .into_iter()
            .map(|(m, r)| {
                let (o, d) = r.unwrap();
                (m, o, d)
            })
            .collect();

        // Cross-mode parity: every mode must produce the same outcome key.
        let keys: Vec<(KernelMode, Result<String, String>)> =
            outs.iter().map(|(m, o, _)| (*m, outcome_key(o))).collect();
        if let Some((m, Err(e))) = keys.iter().find(|(_, k)| k.is_err()) {
            failures.push(format!("{case} [{m:?}] — {e}"));
            continue;
        }
        let keys: Vec<(KernelMode, String)> =
            keys.into_iter().map(|(m, k)| (m, k.unwrap())).collect();
        let (_, ref key0) = keys[0];
        if let Some((m, k)) = keys.iter().find(|(_, k)| k != key0) {
            let (m0, _) = keys[0];
            failures.push(format!(
                "{case} — cross-mode divergence: {m0:?}={key0} vs {m:?}={k}"
            ));
            continue;
        }

        // Outcome expectation (parity holds, so the first mode speaks for all).
        let poisoned = matches!(outs[0].1, Outcome::Poisoned { .. });
        match (t.expect.poisons, poisoned) {
            (false, true) => failures.push(format!(
                "{case} — expected clean, poisoned: {}",
                reason_of(&outs[0].1)
            )),
            (true, false) => failures.push(format!("{case} — expected poison, evaluated clean")),
            _ => {}
        }

        // `#[test(kernel)]`: the body must have dispatched a kernel (Interp mode).
        if t.expect.kernel {
            let interp_dispatches = outs
                .iter()
                .find(|(m, _, _)| *m == KernelMode::Interp)
                .map(|(_, _, d)| *d)
                .unwrap_or(0);
            if interp_dispatches == 0 {
                failures.push(format!(
                    "{case} — #[test(kernel)] but the body never dispatched a kernel"
                ));
            }
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
