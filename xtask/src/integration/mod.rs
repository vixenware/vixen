use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::IsTerminal;
use std::pin::Pin;
use std::time::{Duration, Instant};

use eyre::{Result, bail};
use facet::Facet;
use figue as args;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use tokio::task::{AbortHandle, Id, JoinSet};
use tokio::time;

mod cache_basics;
mod context;
mod error_handling;
mod json_output;
mod lua;
mod multi_crate;
mod scaffold;

use self::context::{BootstrapReport, IntegrationContext, IntegrationRun, ShutdownReport};

const STATUS_WIDTH: usize = 8;

type ScenarioFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
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

    /// Maximum number of scenarios to run in parallel
    #[facet(args::named, default = 8)]
    jobs: usize,
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    run: ScenarioFn,
}

struct RunningScenario {
    name: &'static str,
    started_at: Instant,
    abort_handle: AbortHandle,
}

struct ScenarioOutcome {
    name: &'static str,
    duration: Duration,
    artifact_dir: Option<std::path::PathBuf>,
    result: Result<()>,
}

struct Reporter {
    mode: ReporterMode,
}

enum ReporterMode {
    Fancy(ProgressBar),
    Plain,
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

    let worker_threads = match &cli.command {
        CliCommand::Run(args) => {
            normalized_jobs(args.jobs, args.no_capture, SCENARIOS.len()).max(2)
        }
        CliCommand::List(_) => 1,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
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

    let jobs = normalized_jobs(args.jobs, args.no_capture, selected.len());
    let fail_fast = !args.no_fail_fast;
    let capture_output = !args.no_capture;
    let name_width = selected
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(0)
        .clamp(24, 64);
    let total = selected.len();
    let global_start = Instant::now();
    let mut queue: VecDeque<_> = selected.into_iter().collect();
    let mut running = HashMap::<Id, RunningScenario>::new();
    let mut joinset = JoinSet::new();
    let mut next_id = 0usize;
    let mut passed = 0usize;
    let mut failed = Vec::new();
    let mut cancelled = 0usize;
    let mut stop_scheduling = false;
    let mut interval = time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut reporter = Reporter::new(capture_output);

    let mut run = IntegrationRun::new(capture_output)?;
    reporter.runner_line(
        "BOOT",
        global_start.elapsed(),
        &format!(
            "starting shared daemon + warmup for {} scenario(s), {} job(s)",
            total, jobs
        ),
        None,
        name_width,
    );
    let bootstrap = match run.bootstrap().await {
        Ok(report) => report,
        Err(error) => {
            let kept = run.keep()?;
            reporter.println(&format!(
                "{} {}",
                "daemon artifacts".yellow().bold(),
                kept.display()
            ));
            bail!("failed to bootstrap shared integration daemon: {error:#}");
        }
    };
    emit_bootstrap_report(&mut reporter, Duration::ZERO, &bootstrap, name_width);
    reporter.status_line(
        global_start.elapsed(),
        passed,
        failed.len(),
        cancelled,
        running.len(),
        queue.len(),
        total,
        &running,
    );

    while !queue.is_empty() || !running.is_empty() {
        while !stop_scheduling && running.len() < jobs {
            let Some(scenario) = queue.pop_front() else {
                break;
            };
            let scenario_id = next_id;
            next_id += 1;

            let started_at = Instant::now();
            reporter.runner_line(
                "START",
                global_start.elapsed(),
                scenario.name,
                None,
                name_width,
            );

            let shared = run.shared();
            let abort_handle = joinset.spawn(async move {
                let mut context = IntegrationContext::new(shared, scenario.name, scenario_id)?;
                let result = (scenario.run)(&mut context).await;
                let artifact_dir = if result.is_err() || context::keep_artifacts() {
                    Some(context.keep()?)
                } else {
                    None
                };

                Ok::<_, eyre::Report>(ScenarioOutcome {
                    name: scenario.name,
                    duration: started_at.elapsed(),
                    artifact_dir,
                    result,
                })
            });
            running.insert(
                abort_handle.id(),
                RunningScenario {
                    name: scenario.name,
                    started_at,
                    abort_handle,
                },
            );
        }

        if running.is_empty() {
            continue;
        }

        tokio::select! {
            _ = interval.tick() => {
                reporter.status_line(
                    global_start.elapsed(),
                    passed,
                    failed.len(),
                    cancelled,
                    running.len(),
                    queue.len(),
                    total,
                    &running,
                );
            }
            joined = joinset.join_next_with_id() => {
                let Some(joined) = joined else {
                    break;
                };

                let (task_id, joined) = match joined {
                    Ok(pair) => pair,
                    Err(error) if error.is_cancelled() => {
                        let task_id = error.id();
                        if let Some(info) = running.remove(&task_id) {
                            reporter.runner_line(
                                "CANCEL",
                                global_start.elapsed(),
                                info.name,
                                Some(info.started_at.elapsed()),
                                name_width,
                            );
                        }
                        continue;
                    }
                    Err(error) => {
                        bail!("scenario task panicked: {error}");
                    }
                };

                let Some(info) = running.remove(&task_id) else {
                    bail!("completed task {task_id:?} was not tracked");
                };

                let outcome = joined?;
                match outcome.result {
                    Ok(()) => {
                        passed += 1;
                        reporter.runner_line(
                            "PASS",
                            global_start.elapsed(),
                            info.name,
                            Some(outcome.duration),
                            name_width,
                        );
                        if let Some(artifact_dir) = outcome.artifact_dir {
                            reporter.runner_line(
                                "KEEP",
                                global_start.elapsed(),
                                &format!("{} -> {}", info.name, artifact_dir.display()),
                                None,
                                name_width,
                            );
                        }
                    }
                    Err(error) => {
                        reporter.runner_line(
                            "FAIL",
                            global_start.elapsed(),
                            info.name,
                            Some(outcome.duration),
                            name_width,
                        );
                        if let Some(artifact_dir) = &outcome.artifact_dir {
                            reporter.println(&format!(
                                "{} {}",
                                "artifacts".yellow().bold(),
                                artifact_dir.display()
                            ));
                            reporter.println(&format!(
                                "{} {}",
                                "logs".yellow().bold(),
                                artifact_dir.join("logs").display()
                            ));
                        }
                        reporter.println(&format!("{error:#}"));
                        failed.push((outcome.name, format!("{error:#}")));
                        if fail_fast {
                            cancelled += queue.len() + running.len();
                            queue.clear();
                            stop_scheduling = true;
                            for scenario in running.values() {
                                scenario.abort_handle.abort();
                            }
                        }
                    }
                }
            }
        }
    }

    reporter.runner_line(
        "STOP",
        global_start.elapsed(),
        "stopping shared daemon",
        None,
        name_width,
    );
    let shutdown_started = global_start.elapsed();
    let shutdown_result = run.shutdown().await;
    if let Ok(report) = &shutdown_result {
        emit_shutdown_report(&mut reporter, shutdown_started, report, name_width);
    }
    let run_root = if !failed.is_empty() || context::keep_artifacts() || shutdown_result.is_err() {
        Some(run.keep()?)
    } else {
        None
    };

    if let Some(run_root) = &run_root {
        reporter.runner_line(
            "DAEMON",
            global_start.elapsed(),
            &format!("shared daemon artifacts -> {}", run_root.display()),
            None,
            name_width,
        );
    }

    if let Err(error) = shutdown_result {
        failed.push(("daemon-shutdown", format!("{error:#}")));
    }

    reporter.status_line(
        global_start.elapsed(),
        passed,
        failed.len(),
        cancelled,
        0,
        0,
        total,
        &running,
    );
    reporter.finish();
    reporter.println(&render_summary_line(
        global_start.elapsed(),
        passed,
        failed.len(),
        cancelled,
        total,
    ));

    if failed.is_empty() {
        return Ok(());
    }

    bail!("{} integration scenario(s) failed", failed.len())
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

fn normalized_jobs(requested: usize, no_capture: bool, total: usize) -> usize {
    if no_capture {
        return 1;
    }

    requested.max(1).min(total.max(1))
}

fn render_runner_line(
    status: &str,
    elapsed: Duration,
    name: &str,
    scenario_duration: Option<Duration>,
    name_width: usize,
) -> String {
    let status = format_status_label(status);
    let detail = scenario_duration
        .map(|duration| format!(" ({})", format_duration(duration).dimmed()))
        .unwrap_or_default();

    format!(
        "{} {:>8}  {:<width$}{}",
        status,
        format_duration(elapsed),
        name,
        detail,
        width = name_width,
    )
}

fn render_status_line(
    elapsed: Duration,
    passed: usize,
    failed: usize,
    cancelled: usize,
    running: usize,
    pending: usize,
    total: usize,
    running_map: &HashMap<Id, RunningScenario>,
) -> String {
    let mut running_entries: Vec<_> = running_map.values().collect();
    running_entries.sort_unstable_by_key(|info| info.name);
    let running_preview = if running_entries.is_empty() {
        String::new()
    } else {
        format!(
            "  [{}]",
            running_entries
                .into_iter()
                .take(3)
                .map(|info| format!(
                    "{} {}",
                    info.name,
                    format_duration(info.started_at.elapsed())
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    format!(
        "{} {:>8}  {} passed | {} failed | {} running | {} pending | {} cancelled | {} total{}",
        format_status_label("STATUS"),
        format_duration(elapsed),
        passed.to_string().green(),
        failed.to_string().red(),
        running.to_string().cyan(),
        pending.to_string().yellow(),
        cancelled.to_string().magenta(),
        total.to_string().bold(),
        running_preview.dimmed(),
    )
}

fn render_summary_line(
    elapsed: Duration,
    passed: usize,
    failed: usize,
    cancelled: usize,
    total: usize,
) -> String {
    let padded = format!("{:<STATUS_WIDTH$}", "SUMMARY");
    let status = if failed == 0 {
        padded.green().bold().to_string()
    } else {
        padded.red().bold().to_string()
    };

    format!(
        "{} {:>8}  {} passed | {} failed | {} cancelled | {} total",
        status,
        format_duration(elapsed),
        passed.to_string().green(),
        failed.to_string().red(),
        cancelled.to_string().magenta(),
        total.to_string().bold(),
    )
}

fn format_status_label(status: &str) -> String {
    let padded = format!("{status:<STATUS_WIDTH$}");
    match status {
        "PASS" => padded.green().bold().to_string(),
        "FAIL" => padded.red().bold().to_string(),
        "CANCEL" => padded.yellow().bold().to_string(),
        "START" => padded.cyan().bold().to_string(),
        "BOOT" => padded.blue().bold().to_string(),
        "READY" => padded.blue().bold().to_string(),
        "WARMUP" => padded.blue().bold().to_string(),
        "KEEP" => padded.yellow().bold().to_string(),
        "STOP" => padded.blue().bold().to_string(),
        "KILL" => padded.blue().bold().to_string(),
        "EXIT" => padded.blue().bold().to_string(),
        "GRACE" => padded.blue().bold().to_string(),
        "DAEMON" => padded.magenta().bold().to_string(),
        "STATUS" => padded.dimmed().to_string(),
        _ => padded.bold().to_string(),
    }
}

fn emit_bootstrap_report(
    reporter: &mut Reporter,
    started_at: Duration,
    report: &BootstrapReport,
    name_width: usize,
) {
    let spawned_at = started_at + report.spawn;
    let ready_at = spawned_at + report.ready;
    let warmup_at = ready_at + report.warmup;
    reporter.runner_line(
        "READY",
        spawned_at,
        "shared daemon spawned",
        Some(report.spawn),
        name_width,
    );
    reporter.runner_line(
        "READY",
        ready_at,
        "shared daemon ready",
        Some(report.ready),
        name_width,
    );
    reporter.runner_line(
        "WARMUP",
        warmup_at,
        "warmup build finished",
        Some(report.warmup),
        name_width,
    );
}

fn emit_shutdown_report(
    reporter: &mut Reporter,
    started_at: Duration,
    report: &ShutdownReport,
    name_width: usize,
) {
    let kill_at = started_at + report.kill;
    let exit_at = kill_at + report.wait;
    let grace_at = exit_at + report.grace;
    reporter.runner_line(
        "KILL",
        kill_at,
        "vx kill completed",
        Some(report.kill),
        name_width,
    );
    reporter.runner_line(
        "EXIT",
        exit_at,
        if report.forced {
            "shared daemon exited after forced kill"
        } else {
            "shared daemon exited"
        },
        Some(report.wait),
        name_width,
    );
    reporter.runner_line(
        "GRACE",
        grace_at,
        "post-stop grace sleep",
        Some(report.grace),
        name_width,
    );
}

impl Reporter {
    fn new(enable_redraw: bool) -> Self {
        if enable_redraw && std::io::stderr().is_terminal() {
            let progress = ProgressBar::new_spinner();
            progress.set_draw_target(ProgressDrawTarget::stderr_with_hz(10));
            progress.enable_steady_tick(Duration::from_millis(100));
            progress.set_style(
                ProgressStyle::with_template("{spinner} {msg}")
                    .expect("valid progress template")
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            );
            return Self {
                mode: ReporterMode::Fancy(progress),
            };
        }

        Self {
            mode: ReporterMode::Plain,
        }
    }

    fn runner_line(
        &mut self,
        status: &str,
        elapsed: Duration,
        name: &str,
        scenario_duration: Option<Duration>,
        name_width: usize,
    ) {
        self.println(&render_runner_line(
            status,
            elapsed,
            name,
            scenario_duration,
            name_width,
        ));
    }

    fn status_line(
        &mut self,
        elapsed: Duration,
        passed: usize,
        failed: usize,
        cancelled: usize,
        running: usize,
        pending: usize,
        total: usize,
        running_map: &HashMap<Id, RunningScenario>,
    ) {
        let line = render_status_line(
            elapsed,
            passed,
            failed,
            cancelled,
            running,
            pending,
            total,
            running_map,
        );
        match &self.mode {
            ReporterMode::Fancy(progress) => progress.set_message(line),
            ReporterMode::Plain => println!("{line}"),
        }
    }

    fn println(&mut self, line: &str) {
        match &self.mode {
            ReporterMode::Fancy(progress) => progress.println(line),
            ReporterMode::Plain => println!("{line}"),
        }
    }

    fn finish(&mut self) {
        if let ReporterMode::Fancy(progress) = &self.mode {
            progress.finish_and_clear();
        }
    }
}

fn format_duration(duration: Duration) -> String {
    let total_ms = duration.as_millis();
    let minutes = total_ms / 60_000;
    let seconds = (total_ms % 60_000) / 1_000;
    let tenths = (total_ms % 1_000) / 100;
    format!("{minutes:02}:{seconds:02}.{tenths}")
}

#[cfg(test)]
mod tests {
    use figue as args;

    use super::{Cli, CliCommand, matching_scenarios, normalized_jobs, render_summary_line};

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
        let cli: Cli = args::from_slice(&[
            "run",
            "lua",
            "--no-fail-fast",
            "--no-capture",
            "--jobs",
            "4",
        ])
        .unwrap();
        match cli.command {
            CliCommand::Run(args) => {
                assert_eq!(args.filter.as_deref(), Some("lua"));
                assert!(args.no_fail_fast);
                assert!(args.no_capture);
                assert_eq!(args.jobs, 4);
            }
            CliCommand::List(_) => panic!("expected run command"),
        }
    }

    #[test]
    fn no_capture_forces_serial_execution() {
        assert_eq!(normalized_jobs(8, true, 34), 1);
    }

    #[test]
    fn summary_line_includes_counts() {
        let line = render_summary_line(std::time::Duration::from_secs(7), 4, 1, 29, 34);
        assert!(line.contains("4"));
        assert!(line.contains("1"));
        assert!(line.contains("29"));
        assert!(line.contains("34"));
    }
}
