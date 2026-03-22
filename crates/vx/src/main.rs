//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod build_json;
mod cli;
mod diagnostics;
mod progress_listener;
mod tui;
mod types;

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use camino::Utf8PathBuf;
use eyre::{Result, bail, eyre};
use facet::Facet;
use facet_value::Value as FacetValue;
use figue as args;
use owo_colors::OwoColorize;
use roam_core::Driver;
use roam_types::{ConnectionSettings, Parity};
use tui::emit_osc_progress;
use vx_eval2::{
    CrossingPolicy, EvalError, EvalErrorKind, EvalSession, PendingRef, Provenance, Resolution,
    ReuseClass, RuntimeCallOutcome, RuntimeHooks, SourceSpan, Value as EvalValue, ValueRep,
    value_to_envelope,
};
use vx_eval2_proto::ValueEnvelope;
use vx_io::spawn::{ServiceBuilder, ServiceHandle};
use vx_report::ReportStore;
use vx_store_proto::Blake3Hash;
use vx_tracing::init_cli_stderr;
use vxd_proto::{BuildRequest, PsResult, VxdClient};

use crate::types::{CliContext, CliMode, DaemonStrategy};
use crate::{
    cli::{BuildArgs, CleanArgs, Cli, CliCommand, DebugArgs, EvalArgs, KillArgs, StartArgs},
    progress_listener::ProgressListenerImpl,
};
use vx_io::boot::BootStatusState;

/// Initialize tracing with TUI layer
fn init_tracing_with_tui(tui: tui::TuiHandle) {
    use tracing_subscriber::prelude::*;

    tracing_subscriber::registry()
        .with(vx_tracing::cli_env_filter())
        .with(tui::TuiLayer::new(tui))
        .init();
}

fn main() {
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

    let mut cx = CliContext::from_cli(&cli).expect("failed to create context");

    // Initialize tracing - with TUI layer, or plain output to stderr.
    // In JSON mode, tracing goes to stderr so stdout stays clean NDJSON.
    if let CliMode::Tui(ref tui) = cx.mode {
        init_tracing_with_tui(tui.clone());
    } else {
        init_cli_stderr();
    }

    let result = match cli.command {
        CliCommand::Build(args) => cx.cmd_build(args).await,
        CliCommand::Start(args) => cx.cmd_start(args).await,
        CliCommand::Ping => cx.cmd_ping().await,
        CliCommand::Kill(args) => cx.cmd_kill(args).await,
        CliCommand::Ps => cx.cmd_ps().await,
        CliCommand::Clean(args) => cx.cmd_clean(args).await,
        CliCommand::Doctor(_args) => cx.cmd_doctor().await,
        CliCommand::Debug(args) => cx.cmd_debug(args).await,
        CliCommand::Eval(args) => cx.cmd_eval(args).await,
    };

    if let Err(e) = result {
        if let CliMode::Tui(tui) = &cx.mode {
            tui.shutdown().await;
        }
        print_fatal_error(&e);
        std::process::exit(1);
    }
}

fn print_fatal_error(error: &eyre::Report) {
    // Cap content width: readable at 60, never wider than terminal
    let term_width = terminal_width().saturating_sub(4).max(40);
    let content_width = term_width.min(90);

    let full_message = format!("{error:#}");
    let wrapped = soft_wrap(&full_message, content_width);
    let lines: Vec<&str> = wrapped.lines().collect();
    let bar = "─".repeat(content_width + 2);

    eprintln!();
    eprintln!("{}", format!("┌{bar}┐").red().bold());
    eprintln!(
        "{}",
        format!("│ {:content_width$} │", "FATAL ERROR").red().bold()
    );
    eprintln!("{}", format!("├{bar}┤").red().bold());
    for line in &lines {
        let visible_len = visible_width(line);
        let padding = content_width.saturating_sub(visible_len);
        eprintln!(
            "{}{}{}{}",
            "│ ".red().bold(),
            line,
            " ".repeat(padding),
            " │".red().bold(),
        );
    }
    eprintln!("{}", format!("└{bar}┘").red().bold());
    eprintln!();
}

/// Count visible display width, ignoring ANSI escape sequences.
fn visible_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    let stripped = strip_ansi_escapes::strip(s);
    let plain = String::from_utf8_lossy(&stripped);
    UnicodeWidthStr::width(plain.as_ref())
}

fn terminal_width() -> usize {
    // Try COLUMNS env var first, then ioctl
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(w) = cols.parse::<usize>()
    {
        return w;
    }
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) };
        if ret == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    80
}

fn soft_wrap(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if visible_width(line) <= max_width {
            out.push_str(line);
            out.push('\n');
        } else {
            // Extract the visual indent (leading whitespace after stripping ANSI).
            // We keep the full prefix (ANSI escapes + spaces) so continuations
            // preserve both color and indentation.
            let (indent, indent_width) = extract_indent(line);
            let cont_width = max_width.saturating_sub(indent_width).max(20);

            let mut first = true;
            let mut remaining = line;
            while !remaining.is_empty() {
                // First line uses full width; continuations use reduced width
                // because we prepend the indent prefix.
                let width = if first { max_width } else { cont_width };
                if visible_width(remaining) <= width {
                    out.push_str(remaining);
                    out.push('\n');
                    break;
                }
                let break_pos = find_wrap_point(remaining, width);
                let chunk = &remaining[..break_pos];
                out.push_str(chunk);
                let active_sgr = last_sgr(chunk);
                if active_sgr.is_some() {
                    out.push_str("\x1b[0m");
                }
                out.push('\n');
                remaining = remaining[break_pos..].trim_start();
                first = false;
                // Prepend indent on continuation lines
                if !remaining.is_empty() {
                    out.push_str(indent);
                    if let Some(ref sgr) = active_sgr {
                        out.push_str(sgr);
                    }
                }
            }
        }
    }
    out
}

struct ConnectedVxd {
    handle: ServiceHandle,
    client: VxdClient,
}

impl std::ops::Deref for ConnectedVxd {
    type Target = VxdClient;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

struct LocalEvalRuntime {
    root_dir: PathBuf,
}

impl RuntimeHooks for LocalEvalRuntime {
    fn call(
        &self,
        _place: &str,
        callee: &str,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> std::result::Result<RuntimeCallOutcome, EvalError> {
        match callee {
            "read_file" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "read_file() requires exactly one string argument",
                    ));
                }
                let ValueRep::String(path) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "read_file() argument must evaluate to String",
                    ));
                };
                let full_path = self.root_dir.join(path.as_ref());
                let contents = fs::read_to_string(&full_path).map_err(|error| {
                    EvalError::new(
                        EvalErrorKind::UserFail,
                        format!("read_file() failed for {}: {error}", full_path.display()),
                    )
                })?;
                Ok(RuntimeCallOutcome::Ready(
                    EvalValue::external("Blob", contents)
                        .with_provenance(Provenance::unknown(source.clone()))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

#[derive(Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
#[allow(dead_code)]
enum EvalCliResolution {
    Ready {
        value: ValueEnvelope,
    },
    Pending {
        node_id: u64,
        description: String,
    },
    Waiting,
    Poison {
        kind: String,
        message: String,
        detail: Option<ValueEnvelope>,
    },
}

fn eval_error_report(error: EvalError) -> eyre::Report {
    eyre!("{:?}: {}", error.kind, error.message)
}

/// Extract the leading "indent" from a line that may contain ANSI escapes.
///
/// Returns `(prefix, visible_width)` where `prefix` is the byte slice
/// covering all leading ANSI escapes and whitespace, and `visible_width`
/// is how many display columns the whitespace occupies.
fn extract_indent(line: &str) -> (&str, usize) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut width = 0;

    while i < bytes.len() {
        // Skip ANSI CSI sequences (ESC [ ... final_byte)
        if bytes[i] == b'\x1b' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // skip final byte
            }
            continue;
        }
        // Count spaces/tabs
        if bytes[i] == b' ' {
            width += 1;
            i += 1;
        } else if bytes[i] == b'\t' {
            width += 4; // approximate
            i += 1;
        } else {
            break;
        }
    }

    (&line[..i], width)
}

/// Extract the last SGR (Select Graphic Rendition) sequence from a string,
/// or None if the string ends with a reset or has no SGR sequences.
fn last_sgr(s: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\x1b' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let start = i;
            i += 2;
            while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // skip final byte
                let seq = &s[start..i];
                // Only track SGR sequences (ending with 'm')
                if seq.ends_with('m') {
                    if seq == "\x1b[0m" || seq == "\x1b[m" {
                        last = None; // reset clears active style
                    } else {
                        last = Some(seq.to_string());
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    last
}

/// Find byte offset in `s` where visible width reaches `max_width`,
/// preferring to break at a space. Skips ANSI escape sequences.
fn find_wrap_point(s: &str, max_width: usize) -> usize {
    use unicode_width::UnicodeWidthChar;

    let bytes = s.as_bytes();
    let mut orig_pos = 0;
    let mut col = 0;
    let mut last_space = None;

    while orig_pos < bytes.len() && col < max_width {
        if bytes[orig_pos] == b'\x1b' && orig_pos + 1 < bytes.len() && bytes[orig_pos + 1] == b'[' {
            // Skip CSI sequence: ESC [ ... final_byte
            orig_pos += 2;
            while orig_pos < bytes.len() && !(0x40..=0x7E).contains(&bytes[orig_pos]) {
                orig_pos += 1;
            }
            if orig_pos < bytes.len() {
                orig_pos += 1;
            }
        } else {
            let c = s[orig_pos..].chars().next().unwrap();
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if col + w > max_width {
                break;
            }
            if c == ' ' {
                last_space = Some(orig_pos);
            }
            col += w;
            orig_pos += c.len_utf8();
        }
    }

    last_space.unwrap_or(orig_pos)
}

fn read_eval_source(file: &str) -> Result<(String, String, PathBuf)> {
    if file == "-" {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .map_err(|error| eyre!("failed to read stdin: {error}"))?;
        let root_dir = std::env::current_dir()?;
        return Ok((source, "<stdin>".to_string(), root_dir));
    }

    let cwd = std::env::current_dir()?;
    let path = PathBuf::from(file);
    let full_path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let source = fs::read_to_string(&full_path)
        .map_err(|error| eyre!("failed to read {}: {error}", full_path.display()))?;
    let root_dir = full_path.parent().map(Path::to_path_buf).unwrap_or(cwd);
    Ok((source, full_path.display().to_string(), root_dir))
}

fn parse_eval_args(arg_json: &[String]) -> Result<Vec<EvalValue>> {
    arg_json.iter().map(|arg| parse_eval_arg(arg)).collect()
}

fn parse_eval_arg(arg: &str) -> Result<EvalValue> {
    let value = facet_json::from_str::<FacetValue>(arg)
        .map_err(|error| eyre!("failed to parse --arg-json value {arg:?}: {error}"))?;
    facet_value_to_eval_value(&value)
}

fn facet_value_to_eval_value(value: &FacetValue) -> Result<EvalValue> {
    if value.is_null() {
        return Ok(EvalValue::unit());
    }
    if let Some(value) = value.as_bool() {
        return Ok(EvalValue::bool(value));
    }
    if let Some(value) = value.as_string() {
        return Ok(EvalValue::string(value.as_str()));
    }
    if let Some(value) = value.as_number() {
        return value
            .to_u64()
            .map(EvalValue::u64)
            .ok_or_else(|| eyre!("only non-negative integer JSON numbers are supported"));
    }
    if let Some(items) = value.as_array() {
        return items
            .as_slice()
            .iter()
            .map(facet_value_to_eval_value)
            .collect::<Result<Vec<_>>>()
            .map(EvalValue::array);
    }
    if let Some(entries) = value.as_object() {
        return entries
            .iter()
            .map(|(key, value)| {
                Ok((
                    EvalValue::string(key.as_str()),
                    facet_value_to_eval_value(value)?,
                ))
            })
            .collect::<Result<Vec<_>>>()
            .map(EvalValue::map);
    }

    Err(eyre!(
        "unsupported JSON argument type: {:?}",
        value.value_type()
    ))
}

fn eval_cli_resolution(resolution: Resolution) -> Result<EvalCliResolution> {
    match resolution {
        Resolution::Ready(value) => Ok(EvalCliResolution::Ready {
            value: value_to_envelope(&value).map_err(eval_error_report)?,
        }),
        Resolution::Pending(PendingRef {
            node_id,
            description,
        }) => Ok(EvalCliResolution::Pending {
            node_id,
            description: description.to_string(),
        }),
        Resolution::Waiting => Ok(EvalCliResolution::Waiting),
        Resolution::Poison(error) => Ok(EvalCliResolution::Poison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
            detail: error
                .detail
                .as_ref()
                .map(value_to_envelope)
                .transpose()
                .map_err(eval_error_report)?,
        }),
    }
}

fn print_eval_resolution(mode: &CliMode, resolution: &EvalCliResolution) -> Result<()> {
    let output = match mode {
        CliMode::Json => facet_json::to_string(resolution)
            .map_err(|error| eyre!("serialize eval output: {error}"))?,
        CliMode::Text | CliMode::Tui(_) => facet_json::to_string_pretty(resolution)
            .map_err(|error| eyre!("serialize eval output: {error}"))?,
    };
    println!("{output}");
    Ok(())
}

fn print_eval_reports(args: &EvalArgs, session: &EvalSession<'_, LocalEvalRuntime>) {
    if args.profile {
        eprintln!("{}", session.profile_report(args.profile_limit));
    }
    if args.call_profile || session.call_profiling_enabled() {
        eprintln!("{}", session.call_profile_report(args.profile_limit));
    }
    if args.trace {
        eprintln!("{:#?}", session.trace());
    }
}

// ─── CliContext methods ─────────────────────────────────────────────────────

impl CliContext {
    /// Connect to the daemon, spawning it if necessary.
    ///
    /// The progress dispatcher is created here because it's tightly coupled
    /// to the vx CLI's display logic.
    async fn ensure_vxd(&self) -> Result<ConnectedVxd> {
        let vx_home = vx_io::vx_home();

        // Connect to daemon — root connection only has the Boot service.
        let mut builder =
            ServiceBuilder::new("vxd", self.strat.endpoint().clone()).caller_name("vx");

        builder = match &self.strat {
            DaemonStrategy::Connect(_) => builder.connect_only(),
            DaemonStrategy::Bootstrap(_) => builder
                .env("VX_HOME", vx_home.as_str())
                .log_path(vx_home.join("vxd.log")),
        };

        let handle = builder.connect().await?;

        // Boot protocol: call boot(), then open virtual connection with
        // ProgressListener dispatcher for callbacks from the daemon.
        let boot_client = vx_io::boot::BootClient::new(handle.caller.clone());
        let mut last_phase = None;
        loop {
            let status = boot_client
                .status()
                .await
                .map_err(|e| eyre!("boot status RPC failed: {e:?}"))?;
            match status.state {
                BootStatusState::Booting => {
                    if status.phase != last_phase {
                        let phase = status
                            .phase
                            .clone()
                            .unwrap_or_else(|| "booting services".to_string());
                        tracing::info!("vxd: {phase}");
                        last_phase = status.phase;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                BootStatusState::Ready => break,
                BootStatusState::Failed => {
                    let msg = status
                        .error
                        .unwrap_or_else(|| "unknown boot error".to_string());
                    bail!("{msg}");
                }
            }
        }

        let progress_listener = ProgressListenerImpl::new(self.mode.clone());
        let dispatcher = vxd_proto::ProgressListenerDispatcher::new(progress_listener);
        let virtual_handle = handle
            .session_handle
            .open_connection(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                },
                vec![],
            )
            .await?;
        let mut driver = Driver::new(virtual_handle, dispatcher);
        let caller = driver.caller();
        tokio::spawn(async move { driver.run().await });

        Ok(ConnectedVxd {
            handle,
            client: VxdClient::new(caller),
        })
    }

    // ─── Commands ───────────────────────────────────────────────────────

    async fn cmd_eval(&mut self, args: EvalArgs) -> Result<()> {
        let (source, source_id, root_dir) = read_eval_source(&args.file)?;
        let runtime = LocalEvalRuntime { root_dir };
        let eval_args = parse_eval_args(&args.arg_json)?;
        let mut session = EvalSession::new(
            source,
            source_id,
            args.entry.clone(),
            eval_args,
            &runtime,
            args.place.clone(),
        );
        if args.call_profile {
            session.set_call_profiling_enabled(true);
        }

        let resolution = session.run().map_err(eval_error_report)?;
        let cli_resolution = eval_cli_resolution(resolution.clone())?;
        print_eval_resolution(&self.mode, &cli_resolution)?;
        print_eval_reports(&args, &session);

        match resolution {
            Resolution::Ready(_) => Ok(()),
            Resolution::Pending(_) | Resolution::Waiting | Resolution::Poison(_) => {
                std::process::exit(1);
            }
        }
    }

    async fn cmd_build(&mut self, args: BuildArgs) -> Result<()> {
        let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
        let build_file_path = args.file.as_ref().map(|file| {
            let path = Utf8PathBuf::from(file);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        });

        // Emit indeterminate OSC 9;4 progress while connecting to daemon
        // (TUI mode emits this from its display task; Text mode needs it here)
        // Skip in JSON mode — CI doesn't need terminal progress sequences.
        if matches!(self.mode, CliMode::Text) {
            emit_osc_progress(tui::OscProgressState::Indeterminate, None);
        }

        let daemon = self.ensure_vxd().await?;

        // When running under the test harness, set the session ID so cache keys
        // are namespaced per-test (isolating parallel tests from each other).
        if let Ok(session_id) = std::env::var("VX_TEST_SESSION") {
            daemon
                .set_session(session_id)
                .await
                .map_err(|e| eyre!("set_session RPC failed: {e:?}"))?;
        }

        let mut build_env = Vec::new();
        for entry in &args.env {
            if let Some((key, value)) = entry.split_once('=') {
                build_env.push((key.to_string(), value.to_string()));
            } else {
                let value = std::env::var(entry)
                    .map_err(|_| eyre!("--env {entry}: not set in environment"))?;
                build_env.push((entry.clone(), value));
            }
        }

        let mut request = BuildRequest::new(cwd.clone())
            .release(args.release)
            .env(build_env);
        if let Some(path) = build_file_path {
            request = request.build_file_path(path);
        }

        // Race the build against Ctrl-C. When the user hits Ctrl-C, we shut down
        // the TUI cleanly and exit. The daemon detects the dropped connection and
        // cancels the in-flight build on its own.
        let result = tokio::select! {
            result = daemon.build(request) => result

                .map_err(|e| eyre!("build RPC failed: {e:?}"))?,
            _ = tokio::signal::ctrl_c() => {
                if let CliMode::Tui(tui) = &self.mode {
                    tui.shutdown().await;
                }
                emit_osc_progress(tui::OscProgressState::Remove, None);
                eprintln!("Build interrupted");
                std::process::exit(130);
            }
        };

        // Handle build outcome based on mode
        match &self.mode {
            CliMode::Tui(tui) => {
                tui.set_build_outcome(result.success);
                tui.shutdown().await;
            }
            CliMode::Text => {
                if result.success {
                    emit_osc_progress(tui::OscProgressState::Remove, None);
                } else {
                    emit_osc_progress(tui::OscProgressState::Error, None);
                }
            }
            CliMode::Json => {
                build_json::emit(&build_json::BuildFinished {
                    event: "build_finished".into(),
                    success: result.success,
                    cached: result.cached,
                    duration_ms: result.duration_ms,
                    total_actions: result.total_actions as u64,
                    cache_hits: result.cache_hits as u64,
                    rebuilt: result.rebuilt as u64,
                    cancelled: result.cancelled as u64,
                    output_path: result.output_path.map(|p| p.to_string()),
                    error: result.error.clone(),
                });

                if !result.success {
                    if let Some(ref error) = result.error
                        && let Some(first_line) = error.lines().next()
                    {
                        eprintln!("::error::{first_line}");
                    }
                    std::process::exit(1);
                }

                return Ok(());
            }
        }

        // Text/TUI: print summary or error
        if result.success {
            println!("─────────────────────────────────────────────────────────────");
            println!("{} Build Complete", "✓".green().bold());
            println!("─────────────────────────────────────────────────────────────");

            if result.cached {
                println!(
                    "{} {}",
                    "Status:".dimmed(),
                    "Cached (no rebuild needed)".cyan()
                );
            } else {
                println!("{} {}", "Status:".dimmed(), "Success".green());
            }

            let duration_str = if result.duration_ms < 1000 {
                format!("{}ms", result.duration_ms)
            } else {
                format!("{:.2}s", result.duration_ms as f64 / 1000.0)
            };
            println!("{} {}", "Duration:".dimmed(), duration_str.yellow());

            let executed = result
                .total_actions
                .saturating_sub(result.cache_hits)
                .saturating_sub(result.cancelled);
            if result.cancelled > 0 {
                println!(
                    "{} {} total, {} cached, {} executed, {} cancelled",
                    "Actions:".dimmed(),
                    result.total_actions.to_string().yellow(),
                    result.cache_hits.to_string().cyan(),
                    executed.to_string().green(),
                    result.cancelled.to_string().red()
                );
            } else {
                println!(
                    "{} {} total, {} cached, {} executed",
                    "Actions:".dimmed(),
                    result.total_actions.to_string().yellow(),
                    result.cache_hits.to_string().cyan(),
                    executed.to_string().green()
                );
            }

            if let Some(output_path) = &result.output_path {
                let display_path = if let Ok(rel) = output_path.strip_prefix(&cwd) {
                    rel.to_string()
                } else {
                    output_path.to_string()
                };
                println!("{} {}", "Binary:".dimmed(), display_path.cyan());
            }

            println!("{} {}", "Project:".dimmed(), cwd.to_string().dimmed());
            println!("─────────────────────────────────────────────────────────────");

            Ok(())
        } else {
            if let Some(failure) = &result.failure {
                match diagnostics::render_build_failure(failure, args.full_stack) {
                    Ok(rendered) => eprintln!("{rendered}"),
                    Err(render_error) => {
                        tracing::warn!(error = %render_error, "failed to render build diagnostics");
                        eprintln!("{}", failure.message);
                    }
                }
                std::process::exit(1);
            }
            if let Some(error) = &result.error {
                eprintln!("{error}");
                std::process::exit(1);
            }
            bail!("{}", result.message)
        }
    }

    async fn cmd_start(&mut self, _args: StartArgs) -> Result<()> {
        let mut daemon = self.ensure_vxd().await?;

        tracing::info!("✓ daemon is running");

        // Ping to verify connection
        let version = daemon
            .version()
            .await
            .map_err(|e| eyre!("version RPC failed: {e:?}"))?;

        tracing::info!("✓ connected to vxd");
        tracing::info!("   {}", version);

        if let Some(mut child) = daemon.handle.child.take() {
            let status = child
                .wait()
                .await
                .map_err(|e| eyre!("vxd wait failed: {e}"))?;
            if !status.success() {
                bail!("vxd exited with {status}");
            }
        }

        Ok(())
    }

    async fn cmd_ping(&mut self) -> Result<()> {
        // Ping only connects, never spawns
        self.strat = self.strat.clone().connect_only();
        let daemon = self.ensure_vxd().await?;
        let version = daemon
            .version()
            .await
            .map_err(|e| eyre!("version RPC failed: {e:?}"))?;
        println!("{version}");
        Ok(())
    }

    async fn cmd_ps(&mut self) -> Result<()> {
        self.strat = self.strat.clone().connect_only();
        let daemon = self.ensure_vxd().await?;
        let snapshot = daemon
            .ps()
            .await
            .map_err(|e| eyre!("ps RPC failed: {e:?}"))?;
        print_ps(snapshot);
        Ok(())
    }

    async fn cmd_kill(&mut self, _args: KillArgs) -> Result<()> {
        use vx_io::boot::BootClient;
        use vx_io::spawn::ServiceBuilder;

        // Connect to the root connection only — don't boot, don't open virtual connections.
        // shutdown() lives on the Boot service, so it's always reachable even if boot failed.
        let endpoint = self.strat.endpoint().clone();
        let builder = ServiceBuilder::new("vxd", endpoint.clone())
            .caller_name("vx")
            .timeout(tokio::time::Duration::from_millis(200))
            .connect_only();

        match builder.connect().await {
            Ok(handle) => {
                tracing::info!("Stopping daemon at {}", endpoint);
                let boot_client = BootClient::new(handle.caller.clone());
                if let Err(e) = boot_client.shutdown().await {
                    // shutdown() calls process::exit(0) on the daemon side, so the
                    // connection closing is expected — not an error.
                    tracing::debug!("shutdown RPC returned: {e:?} (expected — daemon exited)");
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                tracing::info!("Daemon stopped");
            }
            Err(_) => {
                tracing::info!(
                    "note: daemon not reachable (child processes use die-with-parent, so they'll exit on their own)",
                );
            }
        }

        Ok(())
    }

    async fn cmd_clean(&mut self, args: CleanArgs) -> Result<()> {
        if args.home && args.cache {
            bail!("--home and --cache are mutually exclusive");
        }

        let vx_home = vx_io::vx_home();

        if args.cache {
            return self.cmd_clean_cache(&vx_home).await;
        }

        // Determine which directory to remove
        let vx_dir = if args.home {
            // Remove VX_HOME (~/.vx)
            vx_home.clone()
        } else {
            // Remove project .vx/
            let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
            cwd.join(".vx")
        };

        if args.home {
            use std::io::{self, Write};

            print!("Remove VX_HOME at {}? [y/N] ", vx_dir);
            io::stdout().flush()?;

            let mut response = String::new();
            io::stdin().read_line(&mut response)?;
            let confirmed = matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes");
            if !confirmed {
                println!("{}", "Aborted.".yellow().bold());
                return Ok(());
            }
        }

        // Then remove the directory if it exists
        if vx_dir.exists() {
            std::fs::remove_dir_all(&vx_dir)?;
            println!("{} {}", "Removed".green().bold(), vx_dir);
        } else {
            println!("{} {} does not exist", "Note:".yellow().bold(), vx_dir);
        }

        Ok(())
    }

    /// Invalidate cache entries from the last build while preserving toolchains
    /// and registry crates.
    async fn cmd_clean_cache(&self, vx_home: &Utf8PathBuf) -> Result<()> {
        let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
        let report_store = ReportStore::new(&cwd);

        let Some(report) = report_store.load_latest().await? else {
            println!(
                "{} No build reports found. Run {} first.",
                "Error:".red().bold(),
                "vx build".cyan()
            );
            return Ok(());
        };

        // Collect cache keys for non-infrastructure nodes
        let mut cache_keys = Vec::new();
        let mut skipped = 0u32;

        for node in &report.nodes {
            if is_infrastructure_node(&node.kind) {
                skipped += 1;
                continue;
            }

            // Use cas_cache_key (the actual store cache key) when available,
            // fall back to cache_key for older reports
            let hex = node.cas_cache_key.as_deref().unwrap_or(&node.cache_key);

            if let Some(key) = Blake3Hash::from_hex(hex) {
                cache_keys.push(key);
            }
        }

        if cache_keys.is_empty() {
            println!(
                "{} No cache entries to invalidate.",
                "Note:".yellow().bold()
            );
            return Ok(());
        }

        // Connect to daemon (spawns it if needed), unpublish via RPC
        let daemon = self.ensure_vxd().await?;

        let result = daemon
            .unpublish_cache_keys(cache_keys)
            .await
            .map_err(|e| eyre!("unpublish_cache_keys RPC failed: {e:?}"))?;

        // Clear persisted picante state so a fresh daemon recomputes on next build.
        // In integration tests, VX_TEST_SESSION is per-test while the daemon is shared,
        // so removing global files here would invalidate other tests racing in parallel.
        if std::env::var("VX_TEST_SESSION").is_err() {
            let picante_cache = vx_home.join("picante.cache");
            let picante_wal = vx_home.join("picante.wal");
            for path in [&picante_cache, &picante_wal] {
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
        }

        println!(
            "{} Invalidated {} cache entries ({} infrastructure preserved, {} already absent)",
            "Done.".green().bold(),
            result.removed,
            skipped,
            result.not_found,
        );
        if result.removed > 0 {
            println!(
                "{}",
                "Next build will re-execute these nodes from scratch.".dimmed()
            );
        }

        Ok(())
    }

    async fn cmd_doctor(&mut self) -> Result<()> {
        // Daemon status — check if vxd socket exists
        let vx_home = vx_io::vx_home();
        println!("{} {}", "home:".bold(), vx_home);
        let daemon_sock = vx_home.join("vxd.sock");
        if daemon_sock.exists() {
            println!(
                "{} {} (socket exists at {})",
                "daemon:".bold(),
                "likely running".green(),
                daemon_sock,
            );
        } else {
            println!(
                "{} {} (will auto-spawn on next build)",
                "daemon:".bold(),
                "not running".dimmed(),
            );
        }

        #[cfg(target_os = "macos")]
        {
            // there used to be stuff here, we might put it back at some point.
        }

        Ok(())
    }

    async fn cmd_debug(&mut self, _args: DebugArgs) -> Result<()> {
        // Launch moire dashboard server CLI.
        let status = moire::process::Command::new("moire")
            .status()
            .await
            .map_err(|e| eyre::eyre!("failed to launch `moire` dashboard CLI: {e}"))?;
        if !status.success() {
            eyre::bail!("`moire` exited with status: {status}");
        }
        Ok(())
    }
}

fn print_ps(snapshot: PsResult) {
    if snapshot.entries.is_empty() {
        println!("no in-flight activity");
        return;
    }

    for entry in snapshot.entries {
        let executor = entry.executor.unwrap_or_else(|| "-".to_string());
        let last_message = entry.last_message.unwrap_or_else(|| "-".to_string());
        println!(
            "[{}] {} {} age={} idle={} {}",
            entry.id,
            entry.kind,
            executor,
            format_duration_ms(entry.age_ms),
            format_duration_ms(entry.idle_ms),
            entry.label
        );
        println!("    last: {last_message}");
        match (entry.remote_idle_ms, entry.last_remote_message) {
            (Some(remote_idle_ms), Some(last_remote_message)) => {
                println!(
                    "    remote: {} ago: {}",
                    format_duration_ms(remote_idle_ms),
                    last_remote_message
                );
            }
            _ => {
                println!(
                    "    remote: none yet ({})",
                    format_duration_ms(entry.age_ms)
                );
            }
        }
    }
}

fn format_duration_ms(ms: u64) -> String {
    if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Returns true if this node kind represents infrastructure (toolchains, registry)
/// that should be preserved during cache invalidation.
fn is_infrastructure_node(kind: &str) -> bool {
    kind.starts_with("toolchain.")
        || kind.starts_with("zig.toolchain.")
        || kind == "registry.acquire"
}

#[cfg(test)]
mod tests;
