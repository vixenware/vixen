use std::io::{self, Write as _};
use std::path::PathBuf;

use eyre::{ContextCompat, Result, bail};
use rodin_core::{append_cargo_evidence_round, render_cargo_evidence_markdown, run_cargo_evidence};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "rodin_core=info".to_owned()))
        .init();

    let mut workspace = PathBuf::from("/Users/amos/oss/facet-cc");
    let mut registry_src =
        PathBuf::from("/Users/amos/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f");
    let mut append = None;
    let mut json_out = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => {
                workspace = PathBuf::from(args.next().context("--workspace requires a path")?);
            }
            "--registry-src" => {
                registry_src =
                    PathBuf::from(args.next().context("--registry-src requires a path")?);
            }
            "--append" => {
                append = Some(PathBuf::from(
                    args.next().context("--append requires a path")?,
                ));
            }
            "--json-out" => {
                json_out = Some(PathBuf::from(
                    args.next().context("--json-out requires a path")?,
                ));
            }
            "--help" | "-h" => {
                writeln!(
                    io::stdout(),
                    "usage: rodin-cargo-evidence [--workspace PATH] [--registry-src PATH] [--append PATH] [--json-out PATH]",
                )?;
                return Ok(());
            }
            _ => bail!("unknown argument {arg}"),
        }
    }

    let report = run_cargo_evidence(&workspace, &registry_src)?;
    if let Some(path) = append {
        append_cargo_evidence_round(&path, &report)?;
    }
    if let Some(path) = json_out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = facet_json::to_string(&report)?;
        std::fs::write(&path, format!("{json}\n"))?;
    }
    io::stdout().write_all(render_cargo_evidence_markdown(&report).as_bytes())?;
    Ok(())
}
