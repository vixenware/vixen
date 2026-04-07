use std::path::Path;

use vx_eval_tests::{EvalResult, ExpectedValue, parse_fixture};
use vx_rt::{RtValue, eval_source};

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let fixture = parse_fixture(&content).map_err(|e| format!("{path:?}: {e}"))?;

    let result =
        eval_source(&fixture.source, &fixture.entry, &[]).map_err(|e| format!("{path:?}: {e}"))?;

    let actual = match &result {
        RtValue::U64(n) => EvalResult::U64(*n),
        RtValue::Bool(b) => EvalResult::Bool(*b),
        RtValue::String(s) => EvalResult::String(s.clone()),
        other => return Err(format!("{path:?}: unexpected result {other:?}").into()),
    };
    let expected = match &fixture.expect {
        ExpectedValue::U64(n) => EvalResult::U64(*n),
        ExpectedValue::Bool(b) => EvalResult::Bool(*b),
        ExpectedValue::String(s) => EvalResult::String(s.clone()),
    };
    if actual != expected {
        return Err(format!("{path:?}: expected {expected:?}, got {actual:?}").into());
    }

    Ok(())
}

datatest_stable::harness! {
    { test = run_fixture, root = "fixtures/slice0", pattern = r"\.styx$" },
}
