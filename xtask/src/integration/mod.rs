use std::future::Future;
use std::pin::Pin;

use eyre::{Result, bail};
use facet::Facet;
use figue as args;

mod cache_basics;
mod context;
mod error_handling;
mod json_output;
mod lua;
mod multi_crate;
mod scaffold;

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

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "lua",
        run: lua::run,
    },
    Scenario {
        name: "cache-basics/fresh-build-succeeds",
        run: cache_basics::fresh_build_succeeds,
    },
    Scenario {
        name: "cache-basics/cargo-compat-build-succeeds-without-build-vx",
        run: cache_basics::cargo_compat_build_succeeds_without_build_vx,
    },
    Scenario {
        name: "cache-basics/build-vx-is-evaluated-and-materialized-into-dot-vx-build",
        run: cache_basics::build_vx_is_evaluated_and_materialized_into_dot_vx_build,
    },
    Scenario {
        name: "cache-basics/build-vx-observer-can-interpret-failed-processes",
        run: cache_basics::build_vx_observer_can_interpret_failed_processes,
    },
    Scenario {
        name: "cache-basics/second-build-is-cache-hit",
        run: cache_basics::second_build_is_cache_hit,
    },
    Scenario {
        name: "cache-basics/source-change-causes-cache-miss",
        run: cache_basics::source_change_causes_cache_miss,
    },
    Scenario {
        name: "cache-basics/profile-change-causes-cache-miss",
        run: cache_basics::profile_change_causes_cache_miss,
    },
    Scenario {
        name: "cache-basics/edition-change-causes-cache-miss",
        run: cache_basics::edition_change_causes_cache_miss,
    },
    Scenario {
        name: "cache-basics/clean-removes-project-local-vx-dir",
        run: cache_basics::clean_removes_project_local_vx_dir,
    },
    Scenario {
        name: "cache-basics/clean-cache-invalidates-last-build",
        run: cache_basics::clean_cache_invalidates_last_build,
    },
    Scenario {
        name: "cache-basics/multi-file-crate-builds-successfully",
        run: cache_basics::multi_file_crate_builds_successfully,
    },
    Scenario {
        name: "cache-basics/module-change-causes-cache-miss",
        run: cache_basics::module_change_causes_cache_miss,
    },
    Scenario {
        name: "cache-basics/different-checkout-path-is-cache-hit",
        run: cache_basics::different_checkout_path_is_cache_hit,
    },
    Scenario {
        name: "error-handling/rejects-registry-deps-without-lockfile",
        run: error_handling::rejects_registry_deps_without_lockfile,
    },
    Scenario {
        name: "error-handling/rejects-workspace",
        run: error_handling::rejects_workspace,
    },
    Scenario {
        name: "error-handling/rejects-build-script",
        run: error_handling::rejects_build_script,
    },
    Scenario {
        name: "error-handling/rejects-features",
        run: error_handling::rejects_features,
    },
    Scenario {
        name: "error-handling/rejects-proc-macros",
        run: error_handling::rejects_proc_macros,
    },
    Scenario {
        name: "error-handling/invalid-build-vx-reports-parse-errors",
        run: error_handling::invalid_build_vx_reports_parse_errors,
    },
    Scenario {
        name: "error-handling/build-vx-requires-tree-output",
        run: error_handling::build_vx_requires_tree_output,
    },
    Scenario {
        name: "json-output/success",
        run: json_output::json_output_success,
    },
    Scenario {
        name: "json-output/cache-hit",
        run: json_output::json_output_cache_hit,
    },
    Scenario {
        name: "json-output/build-error",
        run: json_output::json_output_build_error,
    },
    Scenario {
        name: "json-output/has-action-events",
        run: json_output::json_output_has_action_events,
    },
    Scenario {
        name: "json-output/is-valid-ndjson",
        run: json_output::json_output_is_valid_ndjson,
    },
    Scenario {
        name: "multi-crate/build-succeeds",
        run: multi_crate::multi_crate_build_succeeds,
    },
    Scenario {
        name: "multi-crate/second-build-is-cache-hit",
        run: multi_crate::multi_crate_second_build_is_cache_hit,
    },
    Scenario {
        name: "multi-crate/dep-change-rebuilds-both",
        run: multi_crate::dep_change_rebuilds_both,
    },
    Scenario {
        name: "multi-crate/app-change-rebuilds-only-app",
        run: multi_crate::app_change_rebuilds_only_app,
    },
    Scenario {
        name: "multi-crate/module-rename-invalidates-cache",
        run: multi_crate::module_rename_invalidates_cache,
    },
    Scenario {
        name: "multi-crate/diamond-dependency-builds",
        run: multi_crate::diamond_dependency_builds,
    },
    Scenario {
        name: "multi-crate/diamond-dependency-cache-hit",
        run: multi_crate::diamond_dependency_cache_hit,
    },
    Scenario {
        name: "multi-crate/diamond-shared-dep-change-rebuilds-dependents",
        run: multi_crate::diamond_shared_dep_change_rebuilds_dependents,
    },
];

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
    let shutdown_result = context.shutdown().await;

    match (result, shutdown_result) {
        (Ok(()), Ok(())) => {
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
        (Ok(()), Err(error)) => {
            let kept = context.keep()?;
            eprintln!("Scenario `{}` failed during teardown.", scenario.name);
            eprintln!("Artifacts retained at {}", kept.display());
            eprintln!("Logs are under {}", kept.join("logs").display());
            Err(error.wrap_err(format!(
                "integration scenario `{}` failed while shutting down",
                scenario.name
            )))
        }
        (Err(error), Ok(())) => {
            let kept = context.keep()?;
            eprintln!("Scenario `{}` failed.", scenario.name);
            eprintln!("Artifacts retained at {}", kept.display());
            eprintln!("Logs are under {}", kept.join("logs").display());
            Err(error.wrap_err(format!("integration scenario `{}` failed", scenario.name)))
        }
        (Err(error), Err(shutdown_error)) => {
            let kept = context.keep()?;
            eprintln!("Scenario `{}` failed.", scenario.name);
            eprintln!("Artifacts retained at {}", kept.display());
            eprintln!("Logs are under {}", kept.join("logs").display());
            Err(error
                .wrap_err(format!("integration scenario `{}` failed", scenario.name))
                .wrap_err(format!("failed while shutting down: {shutdown_error:#}")))
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
        let scenarios = matching_scenarios(Some("multi-crate/build-succeeds"));
        assert_eq!(scenarios.len(), 1);
        assert_eq!(scenarios[0].name, "multi-crate/build-succeeds");
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
