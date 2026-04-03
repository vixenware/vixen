//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod cli;

use figue as args;
use vx_services::{
    DAEMON_SOCK, DaemonClient, EvalStrictFunctionRequest, EvalStrictFunctionResult,
    OrchestratorClient, StrictEvalBackend, StrictValue,
};

use crate::cli::{Cli, CliCommand, ClientTargetArgs, EvalArgs};

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
    let address = resolve_daemon_address(&target)?;
    let client: DaemonClient = vox::connect(&address).await?;
    let status = client.status().await?;
    tracing::info!("Daemon status: {status:?}");
    Ok(())
}

async fn eval(args: EvalArgs) -> eyre::Result<()> {
    let source_text = std::fs::read_to_string(args.file.as_std_path())?;
    let backend = parse_backend(&args.backend)?;
    let values = args
        .args
        .iter()
        .map(|arg| parse_value(arg))
        .collect::<eyre::Result<Vec<_>>>()?;

    let address = resolve_daemon_address(&args.target)?;
    let client: OrchestratorClient = vox::connect(&address).await?;
    let result = client
        .eval_strict_function(EvalStrictFunctionRequest {
            source_text,
            function_name: args.function,
            args: values,
            backend,
            include_kernel_dump: args.kernel_dump,
            include_hir: args.hir,
            include_ir: args.ir,
        })
        .await?;

    match result {
        EvalStrictFunctionResult::Ok(success) => {
            println!("{}", format_value(&success.value));
            if let Some(kernel_dump) = success.kernel_dump {
                println!("\n** Kernel\n{kernel_dump}");
            }
            if let Some(hir) = success.hir {
                println!("\n** HIR\n{hir}");
            }
            if let Some(ir) = success.ir {
                println!("\n** IR\n{ir}");
            }
            Ok(())
        }
        EvalStrictFunctionResult::Err(failure) => {
            eprintln!("error: {}", failure.message);
            if let Some(kernel_dump) = failure.kernel_dump {
                eprintln!("\n** Kernel\n{kernel_dump}");
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

fn parse_value(value: &str) -> eyre::Result<StrictValue> {
    if let Some(rest) = value.strip_prefix("u64:") {
        return Ok(StrictValue::U64(rest.parse()?));
    }
    if let Some(rest) = value.strip_prefix("bool:") {
        return match rest {
            "true" => Ok(StrictValue::Bool(true)),
            "false" => Ok(StrictValue::Bool(false)),
            other => eyre::bail!("invalid bool literal `{other}`"),
        };
    }
    if let Some(rest) = value.strip_prefix("string:") {
        return Ok(StrictValue::String(rest.to_owned()));
    }
    Ok(StrictValue::String(value.to_owned()))
}

fn format_value(value: &StrictValue) -> String {
    match value {
        StrictValue::U64(value) => value.to_string(),
        StrictValue::Bool(value) => value.to_string(),
        StrictValue::String(value) => value.clone(),
    }
}

fn resolve_daemon_address(target: &ClientTargetArgs) -> eyre::Result<String> {
    if let Some(address) = &target.daemon {
        return Ok(address.clone());
    }

    if let Some(config_path) = discover_daemon_config_path(target.config.as_ref()) {
        let config = vxd::load_config_file(&config_path)?;
        let Some(address) = config.listen.into_iter().next() else {
            eyre::bail!("daemon config `{config_path}` did not declare any listen addresses");
        };
        return Ok(address);
    }

    Ok(DAEMON_SOCK.to_owned())
}

fn discover_daemon_config_path(
    explicit: Option<&camino::Utf8PathBuf>,
) -> Option<camino::Utf8PathBuf> {
    explicit
        .cloned()
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
    use vx_services::DAEMON_SOCK;

    use crate::cli::ClientTargetArgs;
    use crate::{
        config_home_from_env, daemon_config_path_in, resolve_daemon_address, resolve_store_root,
        state_home_from_env, store_root_in,
    };

    #[test]
    fn explicit_daemon_address_wins() {
        let target = ClientTargetArgs {
            daemon: Some("tcp://daemon-a:5000".to_owned()),
            config: None,
        };

        let address = resolve_daemon_address(&target).expect("address should resolve");
        assert_eq!(address, "tcp://daemon-a:5000");
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
            config: Some(camino::Utf8PathBuf::from_path_buf(config_path).expect("utf-8 path")),
        };

        let address = resolve_daemon_address(&target).expect("address should resolve");
        assert_eq!(address, "local:///tmp/custom.vx");
    }

    #[test]
    fn daemon_address_defaults_to_standard_socket() {
        let target = ClientTargetArgs {
            daemon: None,
            config: None,
        };

        let address = resolve_daemon_address(&target).expect("address should resolve");
        assert_eq!(address, DAEMON_SOCK);
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
}
