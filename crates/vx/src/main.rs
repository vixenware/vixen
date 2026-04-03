//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod cli;

use figue as args;
use vx_services::{
    DAEMON_SOCK, DaemonClient, EvalStrictFunctionRequest, EvalStrictFunctionResult,
    OrchestratorClient, StrictEvalBackend, StrictValue,
};

use crate::cli::{Cli, CliCommand, EvalArgs};

fn main() {
    init_cli_stderr();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(async_main());
}

async fn async_main() {
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

    let res = match cli.command {
        CliCommand::Health(_args) => health().await,
        CliCommand::Eval(args) => eval(args).await,
        CliCommand::Lsp(_args) => vx_lsp::serve().await,
        CliCommand::Daemon(args) => {
            let config = vxd::DaemonConfig::local_all_in_one(
                vec![vx_services::DAEMON_SOCK.to_string()],
                args.store_root,
                args.vfs_mount,
            );
            vxd::serve(config).await
        }
    };

    if let Err(e) = res {
        eprintln!("fatal error: {e}");
        std::process::exit(1);
    }
}

async fn health() -> eyre::Result<()> {
    let client: DaemonClient = vox::connect(DAEMON_SOCK).await?;
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

    let client: OrchestratorClient = vox::connect(DAEMON_SOCK).await?;
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
