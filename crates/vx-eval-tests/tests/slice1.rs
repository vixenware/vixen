use std::path::Path;

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let fixture = vx_eval_tests::parse_fixture(&content).map_err(|e| format!("{path:?}: {e}"))?;
    vx_eval_tests::run_fixture(&fixture).map_err(|e| format!("{path:?}: {e}"))?;
    Ok(())
}

datatest_stable::harness! {
    { test = run_fixture, root = "fixtures/slice1", pattern = r"\.styx$" },
}
