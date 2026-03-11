mod context;
mod lua;

use std::fs;

use eyre::{Context, Result, bail, eyre};
use tempfile::Builder;

use crate::find_workspace_root;

use self::context::{IntegrationContext, keep_artifacts, target_root};

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("lua") => run_named_scenario("lua", lua::run),
        Some(name) => bail!(
            "unknown integration scenario {name:?}\nusage: cargo run -p xtask -- integration <scenario>\navailable scenarios: lua"
        ),
        None => {
            bail!("usage: cargo run -p xtask -- integration <scenario>\navailable scenarios: lua")
        }
    }
}

fn run_named_scenario(
    name: &str,
    scenario: fn(&mut IntegrationContext) -> Result<()>,
) -> Result<()> {
    let workspace_root =
        find_workspace_root().ok_or_else(|| eyre!("Could not find workspace root"))?;
    let target_root = target_root(&workspace_root);
    fs::create_dir_all(&target_root)
        .with_context(|| format!("failed to create {}", target_root.display()))?;

    let tempdir = Builder::new()
        .prefix(&format!("vx-integration-{name}."))
        .tempdir_in(&target_root)
        .with_context(|| format!("failed to create temp dir in {}", target_root.display()))?;

    let mut context = IntegrationContext::new(workspace_root, tempdir)?;

    println!("Running scenario `{name}`");

    let result = scenario(&mut context);
    match result {
        Ok(()) => {
            if keep_artifacts() {
                let kept = context.keep()?;
                println!(
                    "Scenario `{name}` passed. Artifacts retained at {}",
                    kept.display()
                );
            } else {
                println!("Scenario `{name}` passed.");
            }
            Ok(())
        }
        Err(error) => {
            let kept = context.keep()?;
            eprintln!("Scenario `{name}` failed.");
            eprintln!("Artifacts retained at {}", kept.display());
            eprintln!("Logs are under {}", kept.join("logs").display());
            Err(error.wrap_err(format!("integration scenario `{name}` failed")))
        }
    }
}
