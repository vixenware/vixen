use std::future::Future;
use std::pin::Pin;

use eyre::{Result, bail};
use facet::Facet;
use figue as args;

mod context;
mod lua;

use self::context::IntegrationContext;

type ScenarioFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;
type ScenarioFn = for<'a> fn(&'a mut IntegrationContext) -> ScenarioFuture<'a>;

#[derive(Facet, Debug)]
struct Cli {
    #[facet(args::subcommand)]
    command: CliCommand,

    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[derive(Facet, Debug)]
#[repr(u8)]
enum CliCommand {
    /// List available integration scenarios
    List(ListArgs),

    /// Run integration scenarios matching the optional filter
    Run(RunArgs),
}

#[derive(Facet, Debug)]
struct ListArgs {
    /// Optional scenario-name substring filter
    #[facet(args::positional)]
    filter: Option<String>,
}

#[derive(Facet, Debug)]
struct RunArgs {
    /// Optional scenario-name substring filter
    #[facet(args::positional)]
    filter: Option<String>,

    /// Continue running later scenarios after a failure
    #[facet(args::named)]
    no_fail_fast: bool,

    /// Stream scenario stdout/stderr directly instead of capturing command logs
    #[facet(args::named)]
    no_capture: bool,
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    run: ScenarioFn,
}

const SCENARIOS: &[Scenario] = &[Scenario {
    name: "lua",
    run: lua::run,
}];

pub fn run(raw_args: &[String]) -> Result<()> {
    let cli: Cli = args::Driver::new(
        args::builder::<Cli>()
            .expect("failed to build integration CLI")
            .cli(|cli| cli.args(raw_args.iter().cloned()))
            .help(|h| {
                h.program_name("cargo run -p xtask -- integration")
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        match cli.command {
            CliCommand::List(args) => {
                for scenario in matching_scenarios(args.filter.as_deref()) {
                    println!("{}", scenario.name);
                }
                Ok(())
            }
            CliCommand::Run(args) => run_command(args).await,
        }
    })
}

async fn run_command(args: RunArgs) -> Result<()> {
    let selected = matching_scenarios(args.filter.as_deref());
    if selected.is_empty() {
        bail!("no integration scenarios matched the provided filter");
    }

    let mut failures = Vec::new();

    for scenario in selected {
        match run_named_scenario(scenario, !args.no_capture).await {
            Ok(()) => {}
            Err(error) => {
                failures.push((scenario.name, format!("{error:#}")));
                if !args.no_fail_fast {
                    break;
                }
            }
        }
    }

    if failures.is_empty() {
        return Ok(());
    }

    for (name, error) in &failures {
        eprintln!("scenario `{name}` failed:\n{error}");
    }

    bail!("{} integration scenario(s) failed", failures.len())
}

async fn run_named_scenario(scenario: Scenario, capture_output: bool) -> Result<()> {
    let mut context = IntegrationContext::new(scenario.name, capture_output)?;

    println!("Running scenario `{}`", scenario.name);

    let result = (scenario.run)(&mut context).await;
    match result {
        Ok(()) => {
            if context::keep_artifacts() {
                let kept = context.keep()?;
                println!(
                    "Scenario `{}` passed. Artifacts retained at {}",
                    scenario.name,
                    kept.display()
                );
            } else {
                println!("Scenario `{}` passed.", scenario.name);
            }
            Ok(())
        }
        Err(error) => {
            let kept = context.keep()?;
            eprintln!("Scenario `{}` failed.", scenario.name);
            eprintln!("Artifacts retained at {}", kept.display());
            eprintln!("Logs are under {}", kept.join("logs").display());
            Err(error.wrap_err(format!("integration scenario `{}` failed", scenario.name)))
        }
    }
}

fn matching_scenarios(filter: Option<&str>) -> Vec<Scenario> {
    SCENARIOS
        .iter()
        .copied()
        .filter(|scenario| match filter {
            Some(filter) => scenario.name.contains(filter),
            None => true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use figue as args;

    use super::{Cli, CliCommand, matching_scenarios};

    #[test]
    fn filter_matches_by_substring() {
        let scenarios = matching_scenarios(Some("lu"));
        assert_eq!(scenarios.len(), 1);
        assert_eq!(scenarios[0].name, "lua");
    }

    #[test]
    fn cli_parses_list_filter() {
        let cli: Cli = args::from_slice(&["list", "lu"]).unwrap();
        match cli.command {
            CliCommand::List(args) => assert_eq!(args.filter.as_deref(), Some("lu")),
            CliCommand::Run(_) => panic!("expected list command"),
        }
    }

    #[test]
    fn cli_parses_run_filter_and_flags() {
        let cli: Cli = args::from_slice(&["run", "lua", "--no-fail-fast", "--no-capture"]).unwrap();
        match cli.command {
            CliCommand::Run(args) => {
                assert_eq!(args.filter.as_deref(), Some("lua"));
                assert!(args.no_fail_fast);
                assert!(args.no_capture);
            }
            CliCommand::List(_) => panic!("expected run command"),
        }
    }
}
