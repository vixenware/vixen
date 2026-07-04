use std::path::PathBuf;

use eyre::{ContextCompat, Result};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "rodin_core=info".to_owned()))
        .init();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .context("finding workspace root")?
        .to_path_buf();
    let report_path = root.join("docs/design/rodin-c-evidence.md");
    let report = rodin_core::append_round3_report(&report_path)?;
    tracing::info!(
        natural = report.natural.len(),
        pressure = report.pressure.len(),
        "appended Rodin round 3 evidence"
    );
    Ok(())
}
