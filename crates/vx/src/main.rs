//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod cli;

use std::collections::BTreeMap;
use std::fs;
use std::process::Stdio;

use figue as args;
use vx_services::{
    DAEMON_SOCK, DaemonClient, EvalFunctionRequest, EvalFunctionResult, OrchestratorClient,
    RuntimeValue, StrictEvalBackend,
};

use crate::cli::{Cli, CliCommand, ClientTargetArgs, EvalArgs, KillArgs};

fn main() {
    init_cli_stderr();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    if let Err(e) = rt.block_on(async_main()) {
        eprintln!("fatal error: {e}");
        std::process::exit(1);
    }
}

async fn async_main() -> eyre::Result<()> {
    let cli: Cli = args::Driver::new(
        args::builder::<Cli>()
            .expect("failed to build CLI")
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name(env!("CARGO_PKG_NAME"))
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    match cli.command {
        CliCommand::Health(args) => health(args.target).await,
        CliCommand::Kill(args) => kill(args).await,
        CliCommand::Eval(args) => eval(args).await,
        CliCommand::Lsp(_args) => vx_lsp::serve().await,
        CliCommand::Daemon(args) => {
            let config =
                if let Some(config_path) = discover_daemon_config_path(args.config.as_ref()) {
                    vxd::load_config_file(&config_path)?
                } else {
                    vxd::DaemonConfig::local_all_in_one(
                        vec![vx_services::DAEMON_SOCK.to_string()],
                        resolve_store_root(args.store_root.as_ref())?,
                        args.vfs_mount,
                    )
                };
            vxd::serve(config).await
        }
    }
}

async fn health(target: ClientTargetArgs) -> eyre::Result<()> {
    let daemon = ensure_daemon_available(&target, false).await?;
    print!("{}", format_daemon_status(&daemon.address, &daemon.status));
    if let Some(warning) = daemon_binary_warning(&daemon.address, &daemon.status)? {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

async fn kill(args: KillArgs) -> eyre::Result<()> {
    let resolved = resolve_daemon_target(&args.target)?;
    match daemon_status(&resolved.address).await {
        Ok(_) => {
            shutdown_daemon(&resolved.address).await?;
            eprintln!("killed daemon at {}", resolved.address);
            Ok(())
        }
        Err(_) => {
            eprintln!("no daemon running at {}", resolved.address);
            Ok(())
        }
    }
}

async fn shutdown_daemon(address: &str) -> eyre::Result<()> {
    let client: DaemonClient = vox::connect(address).await?;
    client.shutdown().await?;

    for _ in 0..100 {
        if daemon_status(address).await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    eyre::bail!("daemon at {address} did not exit after shutdown request")
}

fn current_executable_blake3() -> eyre::Result<Option<String>> {
    let current_exe = std::env::current_exe()?;
    let bytes = match fs::read(current_exe) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    Ok(Some(blake3::hash(&bytes).to_hex().to_string()))
}

fn daemon_binary_warning(
    address: &str,
    status: &vx_services::DaemonStatus,
) -> eyre::Result<Option<String>> {
    if !address.starts_with("local://") {
        return Ok(None);
    }

    let Some(daemon_hash) = status.executable_blake3.as_deref() else {
        return Ok(None);
    };
    let Some(current_hash) = current_executable_blake3()? else {
        return Ok(None);
    };

    if daemon_hash == current_hash {
        return Ok(None);
    }

    Ok(Some(format!(
        "connected daemon binary differs from this vx client (daemon pid {} path {:?}); run `vx kill` or `vx eval --fresh-daemon`",
        status.pid, status.executable_path
    )))
}

fn format_duration_ms(uptime_ms: u64) -> String {
    let total_seconds = uptime_ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn emit_eval_daemon_note(address: &str, status: &vx_services::DaemonStatus) -> eyre::Result<()> {
    eprintln!(
        "daemon: {} (pid {}, uptime {})",
        address,
        status.pid,
        format_duration_ms(status.uptime_ms)
    );
    if let Some(warning) = daemon_binary_warning(address, status)? {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

async fn eval(args: EvalArgs) -> eyre::Result<()> {
    let source_text = std::fs::read_to_string(args.file.as_std_path())?;
    let backend = parse_backend(&args.backend)?;
    let raw_args = args.args.unwrap_or_default();
    let values = raw_args
        .iter()
        .map(|arg| parse_value(arg))
        .collect::<eyre::Result<Vec<_>>>()?;
    let values = default_eval_args(&args.function, values)?;

    let daemon = ensure_daemon_available(&args.target, args.fresh_daemon).await?;
    emit_eval_daemon_note(&daemon.address, &daemon.status)?;
    let address = daemon.address;
    let client: OrchestratorClient = vox::connect(&address).await?;
    let result = client
        .eval_function(EvalFunctionRequest {
            source_text,
            function_name: args.function,
            args: values,
            backend,
            include_strict_fragment_dump: args.strict_fragment_dump,
            include_hir: args.hir,
            include_ir: args.ir,
        })
        .await?;

    match result {
        EvalFunctionResult::Ok(success) => {
            println!("{}", format_value(&success.value));
            if let Some(strict_fragment_dump) = success.strict_fragment_dump {
                println!("\n** Strict Fragment\n{strict_fragment_dump}");
            }
            if let Some(hir) = success.hir {
                println!("\n** HIR\n{hir}");
            }
            if let Some(ir) = success.ir {
                println!("\n** IR\n{ir}");
            }
            Ok(())
        }
        EvalFunctionResult::Err(failure) => {
            eprintln!("error: {}", failure.message);
            if let Some(strict_fragment_dump) = failure.strict_fragment_dump {
                eprintln!("\n** Strict Fragment\n{strict_fragment_dump}");
            }
            if let Some(hir) = failure.hir {
                eprintln!("\n** HIR\n{hir}");
            }
            if let Some(ir) = failure.ir {
                eprintln!("\n** IR\n{ir}");
            }
            std::process::exit(1);
        }
    }
}

fn parse_backend(value: &str) -> eyre::Result<StrictEvalBackend> {
    match value {
        "interpreter" => Ok(StrictEvalBackend::Interpreter),
        "kajit" => Ok(StrictEvalBackend::Kajit),
        other => eyre::bail!("unsupported backend `{other}`; expected `interpreter` or `kajit`"),
    }
}

fn parse_value(value: &str) -> eyre::Result<RuntimeValue> {
    if let Some(rest) = value.strip_prefix("u64:") {
        return Ok(RuntimeValue::U64(rest.parse()?));
    }
    if let Some(rest) = value.strip_prefix("bool:") {
        return match rest {
            "true" => Ok(RuntimeValue::Bool(true)),
            "false" => Ok(RuntimeValue::Bool(false)),
            other => eyre::bail!("invalid bool literal `{other}`"),
        };
    }
    if let Some(rest) = value.strip_prefix("string:") {
        return Ok(RuntimeValue::String(rest.to_owned()));
    }
    if let Some(rest) = value.strip_prefix("path:") {
        return Ok(RuntimeValue::Path(rest.to_owned()));
    }
    Ok(RuntimeValue::String(value.to_owned()))
}

fn default_eval_args(
    function_name: &str,
    args: Vec<RuntimeValue>,
) -> eyre::Result<Vec<RuntimeValue>> {
    if !args.is_empty() || function_name != "build" {
        return Ok(args);
    }

    let workspace = std::env::current_dir()?;
    let workspace = camino::Utf8PathBuf::from_path_buf(workspace)
        .map_err(|path| eyre::eyre!("workspace path is not valid UTF-8: {}", path.display()))?;

    Ok(vec![
        RuntimeValue::Path(workspace.to_string()),
        RuntimeValue::StringArray(Vec::new()),
        RuntimeValue::StringMap(BTreeMap::new()),
    ])
}

fn format_value(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::U64(value) => value.to_string(),
        RuntimeValue::Bool(value) => value.to_string(),
        RuntimeValue::String(value) => value.clone(),
        RuntimeValue::Path(value) => value.clone(),
        RuntimeValue::StringArray(values) => format!("{values:?}"),
        RuntimeValue::StringMap(values) => format!("{values:?}"),
        RuntimeValue::Tree(hash) => format!("tree:{}", hash.to_hex()),
    }
}

fn format_daemon_status(address: &str, status: &vx_services::DaemonStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!("daemon: {address}\n"));
    out.push_str(&format!("pid: {}\n", status.pid));
    out.push_str(&format!("uptime_ms: {}\n", status.uptime_ms));
    out.push_str(&format!(
        "uptime: {}\n",
        format_duration_ms(status.uptime_ms)
    ));
    if status.listen.is_empty() {
        out.push_str("listen: []\n");
    } else {
        out.push_str("listen:\n");
        for address in &status.listen {
            out.push_str(&format!("  - {address}\n"));
        }
    }
    if status.hosted_services.is_empty() {
        out.push_str("hosted_services: []\n");
    } else {
        out.push_str("hosted_services:\n");
        for service in &status.hosted_services {
            out.push_str(&format!("  - {service}\n"));
        }
    }
    match &status.executable_path {
        Some(path) => out.push_str(&format!("executable_path: {path}\n")),
        None => out.push_str("executable_path: <unknown>\n"),
    }
    match &status.executable_blake3 {
        Some(hash) => out.push_str(&format!("executable_blake3: {hash}\n")),
        None => out.push_str("executable_blake3: <unknown>\n"),
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonTarget {
    address: String,
    auto_start: Option<DaemonLaunch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveDaemon {
    address: String,
    status: vx_services::DaemonStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonLaunch {
    config_path: Option<camino::Utf8PathBuf>,
}

fn resolve_daemon_target(target: &ClientTargetArgs) -> eyre::Result<DaemonTarget> {
    if let Some(address) = &target.daemon {
        return Ok(DaemonTarget {
            address: address.clone(),
            auto_start: None,
        });
    }

    if let Some(config_path) = discover_daemon_config_path(target.config.as_ref()) {
        let config = vxd::load_config_file(&config_path)?;
        let Some(address) = config.listen.into_iter().next() else {
            eyre::bail!("daemon config `{config_path}` did not declare any listen addresses");
        };
        return Ok(DaemonTarget {
            address,
            auto_start: Some(DaemonLaunch {
                config_path: Some(config_path),
            }),
        });
    }

    Ok(DaemonTarget {
        address: DAEMON_SOCK.to_owned(),
        auto_start: Some(DaemonLaunch { config_path: None }),
    })
}

async fn ensure_daemon_available(
    target: &ClientTargetArgs,
    fresh_daemon: bool,
) -> eyre::Result<ActiveDaemon> {
    let resolved = resolve_daemon_target(target)?;

    if let Ok(status) = daemon_status(&resolved.address).await {
        if fresh_daemon {
            let Some(_) = resolved.auto_start.as_ref() else {
                eyre::bail!(
                    "--fresh-daemon requires an auto-startable daemon target, not an explicit --daemon address"
                );
            };
            shutdown_daemon(&resolved.address).await?;
        } else {
            return Ok(ActiveDaemon {
                address: resolved.address,
                status,
            });
        }
    } else if fresh_daemon && resolved.auto_start.is_none() {
        eyre::bail!(
            "--fresh-daemon requires an auto-startable daemon target, not an explicit --daemon address"
        );
    }

    let Some(launch) = resolved.auto_start.as_ref() else {
        eyre::bail!("failed to connect to daemon at {}", resolved.address);
    };

    spawn_daemon(launch)?;
    let status = daemon_status(&resolved.address).await?;
    Ok(ActiveDaemon {
        address: resolved.address,
        status,
    })
}

async fn daemon_status(address: &str) -> eyre::Result<vx_services::DaemonStatus> {
    let client: DaemonClient = vox::connect(address).await?;
    Ok(client.status().await?)
}

fn spawn_daemon(launch: &DaemonLaunch) -> eyre::Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg("daemon");
    if let Some(config_path) = &launch.config_path {
        command.arg("--config");
        command.arg(config_path.as_str());
    }
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.spawn()?;
    Ok(())
}

fn discover_daemon_config_path(
    explicit: Option<&camino::Utf8PathBuf>,
) -> Option<camino::Utf8PathBuf> {
    explicit
        .cloned()
        .or_else(|| {
            daemon_config_override_from_env(std::env::var_os("VX_DAEMON_CONFIG").as_deref())
        })
        .or_else(|| default_daemon_config_path().filter(|path| path.exists()))
}

fn default_daemon_config_path() -> Option<camino::Utf8PathBuf> {
    default_config_home().map(|root| daemon_config_path_in(&root))
}

fn resolve_store_root(explicit: Option<&camino::Utf8PathBuf>) -> eyre::Result<camino::Utf8PathBuf> {
    explicit
        .cloned()
        .or_else(default_store_root)
        .ok_or_else(|| {
            eyre::eyre!("could not determine store root; pass --store-root or use a daemon config")
        })
}

fn default_store_root() -> Option<camino::Utf8PathBuf> {
    default_state_home().map(|root| store_root_in(&root))
}

fn default_config_home() -> Option<camino::Utf8PathBuf> {
    config_home_from_env(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn default_state_home() -> Option<camino::Utf8PathBuf> {
    state_home_from_env(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn config_home_from_env(
    xdg_config_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<camino::Utf8PathBuf> {
    xdg_config_home
        .and_then(|path| camino::Utf8PathBuf::from_path_buf(path.into()).ok())
        .or_else(|| {
            home.and_then(|path| {
                camino::Utf8PathBuf::from_path_buf(std::path::PathBuf::from(path))
                    .ok()
                    .map(|home| home.join(".config"))
            })
        })
}

fn state_home_from_env(
    xdg_state_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<camino::Utf8PathBuf> {
    xdg_state_home
        .and_then(|path| camino::Utf8PathBuf::from_path_buf(path.into()).ok())
        .or_else(|| {
            home.and_then(|path| {
                camino::Utf8PathBuf::from_path_buf(std::path::PathBuf::from(path))
                    .ok()
                    .map(|home| home.join(".local/state"))
            })
        })
}

fn daemon_config_path_in(config_home: &camino::Utf8PathBuf) -> camino::Utf8PathBuf {
    config_home.join("vx/daemon.styx")
}

fn daemon_config_override_from_env(value: Option<&std::ffi::OsStr>) -> Option<camino::Utf8PathBuf> {
    value.and_then(|path| camino::Utf8PathBuf::from_path_buf(std::path::PathBuf::from(path)).ok())
}

fn store_root_in(state_home: &camino::Utf8PathBuf) -> camino::Utf8PathBuf {
    state_home.join("vx/store")
}

/// Build the default EnvFilter for CLI processes that talk to multiple vx crates.
pub fn cli_env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "warn,vx=info,vxd=info,vx_store=info,vx_toolchain=info,vx_runner=info",
        )
    })
}

/// Initialize tracing for CLI with plain stderr output (non-TUI mode).
pub fn init_cli_stderr() {
    tracing_subscriber::fmt()
        .with_timer(tracing_subscriber::fmt::time::SystemTime)
        .with_env_filter(cli_env_filter())
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_ids(false)
        .init();
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use tempfile::TempDir;
    use vx_services::{DAEMON_SOCK, DaemonStatus, RuntimeValue};

    use crate::cli::{ClientTargetArgs, EvalArgs};
    use crate::{
        config_home_from_env, current_executable_blake3, daemon_binary_warning,
        daemon_config_override_from_env, daemon_config_path_in, default_eval_args,
        format_daemon_status, resolve_daemon_target, resolve_store_root, state_home_from_env,
        store_root_in,
    };

    #[test]
    fn explicit_daemon_address_wins() {
        let target = ClientTargetArgs {
            daemon: Some("tcp://daemon-a:5000".to_owned()),
            config: None,
        };

        let address = resolve_daemon_target(&target)
            .expect("target should resolve")
            .address;
        assert_eq!(address, "tcp://daemon-a:5000");
        let resolved = resolve_daemon_target(&target).expect("target should resolve");
        assert_eq!(resolved.auto_start, None);
    }

    #[test]
    fn daemon_address_comes_from_config_when_present() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_path = temp_dir.path().join("vxd.styx");
        std::fs::write(
            &config_path,
            r#"
daemon {
  listen ("local:///tmp/custom.vx")
  services {
    orchestrator {}
  }
}
"#,
        )
        .expect("failed to write config");

        let target = ClientTargetArgs {
            daemon: None,
            config: Some(
                camino::Utf8PathBuf::from_path_buf(config_path.clone()).expect("utf-8 path"),
            ),
        };

        let address = resolve_daemon_target(&target)
            .expect("target should resolve")
            .address;
        assert_eq!(address, "local:///tmp/custom.vx");
        let resolved = resolve_daemon_target(&target).expect("target should resolve");
        assert_eq!(
            resolved.auto_start,
            Some(super::DaemonLaunch {
                config_path: Some(
                    camino::Utf8PathBuf::from_path_buf(config_path).expect("utf-8 path")
                ),
            })
        );
    }

    #[test]
    fn daemon_address_defaults_to_standard_socket() {
        let target = ClientTargetArgs {
            daemon: None,
            config: None,
        };

        let address = resolve_daemon_target(&target)
            .expect("target should resolve")
            .address;
        assert_eq!(address, DAEMON_SOCK);
        let resolved = resolve_daemon_target(&target).expect("target should resolve");
        assert_eq!(
            resolved.auto_start,
            Some(super::DaemonLaunch { config_path: None })
        );
    }

    #[test]
    fn discovers_well_known_daemon_config_path() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config_home =
            camino::Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf()).expect("utf-8 path");
        assert_eq!(
            daemon_config_path_in(&config_home),
            camino::Utf8PathBuf::from_path_buf(temp_dir.path().join("vx/daemon.styx"))
                .expect("utf-8 path")
        );
    }

    #[test]
    fn config_home_uses_xdg_then_home_fallback() {
        assert_eq!(
            config_home_from_env(Some(OsStr::new("/tmp/cfg")), Some(OsStr::new("/home/amos"))),
            Some(camino::Utf8PathBuf::from("/tmp/cfg"))
        );
        assert_eq!(
            config_home_from_env(None, Some(OsStr::new("/home/amos"))),
            Some(camino::Utf8PathBuf::from("/home/amos/.config"))
        );
    }

    #[test]
    fn daemon_config_override_comes_from_env_var() {
        assert_eq!(
            daemon_config_override_from_env(Some(OsStr::new("/tmp/vxd.styx"))),
            Some(camino::Utf8PathBuf::from("/tmp/vxd.styx"))
        );
    }

    #[test]
    fn state_home_uses_xdg_then_home_fallback() {
        assert_eq!(
            state_home_from_env(
                Some(OsStr::new("/tmp/state")),
                Some(OsStr::new("/home/amos"))
            ),
            Some(camino::Utf8PathBuf::from("/tmp/state"))
        );
        assert_eq!(
            state_home_from_env(None, Some(OsStr::new("/home/amos"))),
            Some(camino::Utf8PathBuf::from("/home/amos/.local/state"))
        );
    }

    #[test]
    fn resolve_store_root_uses_explicit_value() {
        let store_root = camino::Utf8PathBuf::from("/tmp/vx-store");
        let resolved = resolve_store_root(Some(&store_root)).expect("store root should resolve");
        assert_eq!(resolved, store_root);
    }

    #[test]
    fn store_root_path_is_derived_from_state_home() {
        let state_home = camino::Utf8PathBuf::from("/tmp/state");
        assert_eq!(
            store_root_in(&state_home),
            camino::Utf8PathBuf::from("/tmp/state/vx/store")
        );
    }

    #[test]
    fn formats_daemon_status_with_inventory() {
        let rendered = format_daemon_status(
            "local:///tmp/vixen.vox",
            &DaemonStatus {
                pid: 42,
                uptime_ms: 42,
                listen: vec!["local:///tmp/vixen.vox".to_owned()],
                hosted_services: vec!["Daemon".to_owned(), "Orchestrator".to_owned()],
                executable_path: Some("/Users/amos/.cargo/bin/vx".to_owned()),
                executable_blake3: Some("abc123".to_owned()),
            },
        );

        assert!(rendered.contains("daemon: local:///tmp/vixen.vox"));
        assert!(rendered.contains("pid: 42"));
        assert!(rendered.contains("uptime_ms: 42"));
        assert!(rendered.contains("uptime: 0s"));
        assert!(rendered.contains("listen:\n  - local:///tmp/vixen.vox"));
        assert!(rendered.contains("hosted_services:\n  - Daemon\n  - Orchestrator"));
        assert!(rendered.contains("executable_path: /Users/amos/.cargo/bin/vx"));
        assert!(rendered.contains("executable_blake3: abc123"));
    }

    #[test]
    fn warns_when_local_daemon_binary_differs() {
        let status = DaemonStatus {
            pid: 7,
            uptime_ms: 1_000,
            listen: vec!["local:///tmp/vixen.vox".to_owned()],
            hosted_services: vec!["Daemon".to_owned()],
            executable_path: Some("/tmp/old-vx".to_owned()),
            executable_blake3: Some("definitely-not-current".to_owned()),
        };

        let warning = daemon_binary_warning("local:///tmp/vixen.vox", &status)
            .expect("warning should resolve");
        assert!(warning.is_some());
        assert!(
            warning
                .expect("warning should be present")
                .contains("vx eval --fresh-daemon")
        );
    }

    #[test]
    fn does_not_warn_when_local_daemon_binary_matches() {
        let status = DaemonStatus {
            pid: 7,
            uptime_ms: 1_000,
            listen: vec!["local:///tmp/vixen.vox".to_owned()],
            hosted_services: vec!["Daemon".to_owned()],
            executable_path: Some("/tmp/current-vx".to_owned()),
            executable_blake3: current_executable_blake3().expect("hash should resolve"),
        };

        let warning = daemon_binary_warning("local:///tmp/vixen.vox", &status)
            .expect("warning should resolve");
        assert!(warning.is_none());
    }

    #[test]
    fn build_defaults_supply_standard_entry_arguments() {
        let args = default_eval_args("build", Vec::new()).expect("build defaults should resolve");
        assert_eq!(args.len(), 3);
        assert!(matches!(args[0], RuntimeValue::Path(_)));
        assert_eq!(args[1], RuntimeValue::StringArray(Vec::new()));
        assert_eq!(
            args[2],
            RuntimeValue::StringMap(std::collections::BTreeMap::new())
        );
    }

    #[test]
    fn non_build_defaults_do_not_inject_arguments() {
        let args = default_eval_args("greet", Vec::new()).expect("defaults should resolve");
        assert!(args.is_empty());
    }

    #[test]
    fn eval_args_can_request_fresh_daemon() {
        let args = EvalArgs {
            target: ClientTargetArgs {
                daemon: None,
                config: None,
            },
            fresh_daemon: true,
            file: camino::Utf8PathBuf::from("zoo/rustc-hello.vx"),
            function: "build".to_owned(),
            backend: "interpreter".to_owned(),
            strict_fragment_dump: false,
            hir: false,
            ir: false,
            args: None,
        };

        assert!(args.fresh_daemon);
    }
}
