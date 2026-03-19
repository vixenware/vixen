use std::path::Path;

use vx_spec::{EngineExpectation, EngineId, SpecCase};

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let case = SpecCase::from_path(path)?;
    match case.expectation_for(EngineId::Eval2) {
        EngineExpectation::Pass => Ok(()),
        EngineExpectation::Todo | EngineExpectation::Skip => Ok(()),
    }
}

datatest_stable::harness! {
    { test = run_fixture, root = "../../specs/eval", pattern = r"(^|.*/)[^/]+\.styx$" },
}
