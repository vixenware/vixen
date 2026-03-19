use std::path::Path;

use vx_spec::SpecCase;

fn parse_fixture(path: &Path) -> datatest_stable::Result<()> {
    let _case = SpecCase::from_path(path)?;
    Ok(())
}

datatest_stable::harness! {
    { test = parse_fixture, root = "../../specs/eval", pattern = r"(^|.*/)[^/]+\.styx$" },
}
