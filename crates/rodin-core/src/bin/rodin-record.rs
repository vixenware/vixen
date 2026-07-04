use std::io::{self, Write};
use std::path::PathBuf;

use eyre::{ContextCompat, Result, bail};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "rodin_core=info".to_owned()))
        .init();

    let mut args = std::env::args().skip(1);
    let Some(first) = args.next() else {
        bail!("usage: rodin-record <scenario> --out <path>");
    };
    if first == "--list" {
        let mut stdout = io::stdout().lock();
        for name in rodin_core::round3_pressure_scenario_names()? {
            writeln!(stdout, "{name}")?;
        }
        return Ok(());
    }

    let mut out = None;
    while let Some(arg) = args.next() {
        if arg == "--out" {
            out = Some(PathBuf::from(
                args.next().context("--out requires an output path")?,
            ));
        } else {
            bail!("unknown argument {arg}");
        }
    }
    let out = out.context("missing --out <path>")?;
    let record = rodin_core::write_round3_record(&first, &out)?;
    tracing::info!(
        scenario = record.scenario,
        steps = record.steps.len(),
        facts = record.facts.len(),
        path = %out.display(),
        "wrote Rodin solve record"
    );
    Ok(())
}
