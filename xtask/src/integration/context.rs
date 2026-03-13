use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use eyre::{Context, Result, bail, eyre};
use owo_colors::OwoColorize;
use tempfile::{Builder, TempDir};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, timeout};

use crate::find_workspace_root;

const REQUIRED_BINARIES: &[&str] = &["vx", "vxd", "vx-runner", "vx-store", "vx-vfsd"];
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

pub(crate) struct IntegrationRun {
    tempdir: Option<TempDir>,
    root_dir: PathBuf,
    daemon_logs_dir: PathBuf,
    shared: Arc<SharedRun>,
    daemon: Option<DaemonGuard>,
}

pub(crate) struct IntegrationContext {
    tempdir: Option<TempDir>,
    root_dir: PathBuf,
    project_dir: PathBuf,
    logs_dir: PathBuf,
    path_env: OsString,
    session_id: String,
    command_seq: usize,
    shared: Arc<SharedRun>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BuildOptions {
    release: bool,
    json: bool,
    subdir: Option<String>,
}

pub(crate) struct CommandOutput {
    status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    merged_output: String,
}

pub(crate) struct BootstrapReport {
    pub(crate) spawn: Duration,
    pub(crate) ready: Duration,
    pub(crate) warmup: Duration,
}

pub(crate) struct ShutdownReport {
    pub(crate) kill: Duration,
    pub(crate) wait: Duration,
    pub(crate) forced: bool,
    pub(crate) grace: Duration,
}

struct DaemonGuard {
    child: Child,
    log_path: PathBuf,
    stopped: bool,
    vx_binary: PathBuf,
    vx_home: PathBuf,
}

pub(crate) struct SharedRun {
    vx_home: PathBuf,
    vx_binary: PathBuf,
    vxd: PathBuf,
    http_cache_dir: PathBuf,
    capture_output: bool,
    keep_temp: bool,
}

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

struct StreamItem {
    stream: StreamKind,
    line: String,
}

impl BuildOptions {
    pub(crate) fn release(mut self) -> Self {
        self.release = true;
        self
    }

    pub(crate) fn json(mut self) -> Self {
        self.json = true;
        self
    }

    pub(crate) fn subdir(mut self, subdir: &str) -> Self {
        self.subdir = Some(subdir.to_owned());
        self
    }
}

impl IntegrationRun {
    pub(crate) fn new(capture_output: bool, keep_temp: bool) -> Result<Self> {
        let workspace_root =
            find_workspace_root().ok_or_else(|| eyre!("Could not find workspace root"))?;
        ensure_workspace_binaries(&workspace_root)?;

        let temp_root = run_temp_root();
        fs::create_dir_all(&temp_root)
            .with_context(|| format!("failed to create {}", temp_root.display()))?;

        let tempdir = Builder::new()
            .prefix("run.")
            .tempdir_in(&temp_root)
            .with_context(|| format!("failed to create temp dir in {}", temp_root.display()))?;

        let root_dir = tempdir.path().to_path_buf();
        let daemon_logs_dir = root_dir.join("logs");
        let vx_home = root_dir.join("vx-home");
        for dir in [&daemon_logs_dir, &vx_home] {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }

        let vx_binary = target_root(&workspace_root)
            .join("debug")
            .join(binary_name("vx"));
        if !vx_binary.exists() {
            bail!("expected vx binary at {}", vx_binary.display());
        }

        let shared = Arc::new(SharedRun {
            vxd: vx_home.join(endpoint_name("vxd")),
            vx_home,
            vx_binary,
            http_cache_dir: http_cache_dir(),
            capture_output,
            keep_temp,
        });

        Ok(Self {
            tempdir: Some(tempdir),
            root_dir,
            daemon_logs_dir,
            shared,
            daemon: None,
        })
    }

    pub(crate) async fn bootstrap(&mut self) -> Result<BootstrapReport> {
        if self.daemon.is_some() {
            return Ok(BootstrapReport {
                spawn: Duration::ZERO,
                ready: Duration::ZERO,
                warmup: Duration::ZERO,
            });
        }

        let spawn_started = Instant::now();
        let mut daemon = self.start_daemon().await?;
        let spawn = spawn_started.elapsed();

        let ready_started = Instant::now();
        self.wait_for_daemon(&mut daemon).await?;
        let ready = ready_started.elapsed();

        let warmup_started = Instant::now();
        self.warmup_toolchain().await?;
        let warmup = warmup_started.elapsed();
        self.daemon = Some(daemon);
        Ok(BootstrapReport {
            spawn,
            ready,
            warmup,
        })
    }

    pub(crate) fn shared(&self) -> Arc<SharedRun> {
        Arc::clone(&self.shared)
    }

    pub(crate) async fn shutdown(&mut self) -> Result<ShutdownReport> {
        if let Some(mut daemon) = self.daemon.take() {
            let mut report = daemon.stop().await?;
            let grace_started = Instant::now();
            sleep(SHUTDOWN_GRACE).await;
            report.grace = grace_started.elapsed();
            return Ok(report);
        }
        Ok(ShutdownReport {
            kill: Duration::ZERO,
            wait: Duration::ZERO,
            forced: false,
            grace: Duration::ZERO,
        })
    }

    pub(crate) fn keep(&mut self) -> Result<PathBuf> {
        if let Some(tempdir) = self.tempdir.take() {
            return Ok(tempdir.keep());
        }
        Ok(self.root_dir.clone())
    }

    async fn start_daemon(&self) -> Result<DaemonGuard> {
        let log_path = self.daemon_logs_dir.join("vxd.log");
        let stdout = File::create(&log_path)
            .with_context(|| format!("failed to create {}", log_path.display()))?;
        let stderr = OpenOptions::new()
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to reopen {}", log_path.display()))?;

        let mut cmd = Command::new(&self.shared.vx_binary);
        cmd.current_dir(&self.root_dir);
        cmd.env("VX_HOME", &self.shared.vx_home);
        cmd.env("VX_HTTP_CACHE_DIR", &self.shared.http_cache_dir);
        cmd.env("VX_TEST", "1");
        cmd.env_remove("VXD");
        cmd.arg("start");
        cmd.arg("--no-tui");
        cmd.kill_on_drop(true);
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));

        let child = cmd.spawn().context("failed to start vxd")?;

        Ok(DaemonGuard {
            child,
            log_path,
            stopped: false,
            vx_binary: self.shared.vx_binary.clone(),
            vx_home: self.shared.vx_home.clone(),
        })
    }

    async fn wait_for_daemon(&self, daemon: &mut DaemonGuard) -> Result<()> {
        let ping_log = self.daemon_logs_dir.join("ping.log");
        let deadline = Instant::now() + READINESS_TIMEOUT;

        while Instant::now() < deadline {
            if let Some(status) = daemon.child.try_wait().context("failed to poll vxd")? {
                bail!(
                    "vxd exited with {status} before becoming ready at {}\n--- daemon log ({}) ---\n{}",
                    self.shared.vxd.display(),
                    daemon.log_path.display(),
                    read_log(&daemon.log_path)
                );
            }

            let mut cmd = Command::new(&self.shared.vx_binary);
            cmd.current_dir(&self.root_dir);
            cmd.env("VX_HOME", &self.shared.vx_home);
            cmd.env("VX_HTTP_CACHE_DIR", &self.shared.http_cache_dir);
            cmd.env("VX_TEST", "1");
            cmd.env("VXD", &self.shared.vxd);
            cmd.arg("ping");

            let status =
                run_status_with_timeout(&mut cmd, Duration::from_secs(2), &ping_log, "vx ping")
                    .await?;
            if status.success() {
                return Ok(());
            }

            sleep(Duration::from_millis(100)).await;
        }

        bail!(
            "vxd did not become ready within {:?}\n--- daemon log ({}) ---\n{}",
            READINESS_TIMEOUT,
            daemon.log_path.display(),
            read_log(&daemon.log_path)
        )
    }

    async fn warmup_toolchain(&self) -> Result<()> {
        let warmup_dir = self.root_dir.join("warmup");
        let src_dir = warmup_dir.join("src");
        fs::create_dir_all(&src_dir)
            .with_context(|| format!("failed to create {}", src_dir.display()))?;
        fs::write(
            warmup_dir.join("Cargo.toml"),
            "[package]\nname = \"warmup\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .with_context(|| {
            format!(
                "failed to write {}",
                warmup_dir.join("Cargo.toml").display()
            )
        })?;
        fs::write(src_dir.join("main.rs"), "fn main() {}\n")
            .with_context(|| format!("failed to write {}", src_dir.join("main.rs").display()))?;

        let stdout_log = self.daemon_logs_dir.join("warmup.stdout.log");
        let stderr_log = self.daemon_logs_dir.join("warmup.stderr.log");
        let mut cmd = Command::new(&self.shared.vx_binary);
        cmd.current_dir(&warmup_dir);
        cmd.env("VX_HOME", &self.shared.vx_home);
        cmd.env("VX_HTTP_CACHE_DIR", &self.shared.http_cache_dir);
        cmd.env("VX_TEST", "1");
        cmd.env("VX_TEST_SESSION", "warmup");
        cmd.env("VXD", &self.shared.vxd);
        cmd.arg("--no-tui");
        cmd.arg("build");
        cmd.kill_on_drop(true);
        cmd.stdout(Stdio::from(File::create(&stdout_log).with_context(
            || format!("failed to create {}", stdout_log.display()),
        )?));
        cmd.stderr(Stdio::from(File::create(&stderr_log).with_context(
            || format!("failed to create {}", stderr_log.display()),
        )?));

        let status = timeout(COMMAND_TIMEOUT, cmd.status())
            .await
            .map_err(|_| {
                eyre!(
                    "warmup build timed out after {:?}\nstdout log: {}\nstderr log: {}",
                    COMMAND_TIMEOUT,
                    stdout_log.display(),
                    stderr_log.display()
                )
            })?
            .context("failed to run warmup build")?;

        if status.success() {
            return Ok(());
        }

        bail!(
            "warmup build failed with {status}\nstdout log: {}\nstderr log: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout_log.display(),
            stderr_log.display(),
            read_log(&stdout_log),
            read_log(&stderr_log),
        );
    }
}

impl IntegrationContext {
    pub(crate) fn new(shared: Arc<SharedRun>, name: &str, scenario_id: usize) -> Result<Self> {
        let temp_root = scenario_temp_root();
        fs::create_dir_all(&temp_root)
            .with_context(|| format!("failed to create {}", temp_root.display()))?;

        let tempdir = Builder::new()
            .prefix(&format!("vxi-{}.", tempdir_scenario_slug(name)))
            .tempdir_in(&temp_root)
            .with_context(|| format!("failed to create temp dir in {}", temp_root.display()))?;

        let root_dir = tempdir.path().to_path_buf();
        let project_dir = root_dir.join("project");
        let logs_dir = root_dir.join("logs");
        for dir in [&project_dir, &logs_dir] {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }

        let path_env = env::var_os("PATH").unwrap_or_default();
        let session_id = format!("scenario-pid{}-{scenario_id}", std::process::id());

        Ok(Self {
            tempdir: Some(tempdir),
            root_dir,
            project_dir,
            logs_dir,
            path_env,
            session_id,
            command_seq: 0,
            shared,
        })
    }

    pub(crate) fn keep(&mut self) -> Result<PathBuf> {
        if let Some(tempdir) = self.tempdir.take() {
            return Ok(tempdir.keep());
        }
        Ok(self.root_dir.clone())
    }

    pub(crate) fn should_keep(&self) -> bool {
        self.shared.keep_temp
    }

    pub(crate) fn prepend_path(&mut self, dir: &Path) -> Result<()> {
        let joined = env::join_paths(
            std::iter::once(dir.to_path_buf()).chain(env::split_paths(&self.path_env)),
        )
        .with_context(|| format!("failed to prepend {} to PATH", dir.display()))?;
        self.path_env = joined;
        Ok(())
    }

    pub(crate) fn write_project_file(&self, relative_path: &str, contents: &str) -> Result<()> {
        self.write_file(&self.project_dir, relative_path, contents)
    }

    pub(crate) fn project_path(&self, relative_path: &str) -> PathBuf {
        self.project_dir.join(relative_path)
    }

    pub(crate) fn file_exists(&self, relative_path: &str) -> bool {
        self.project_dir.join(relative_path).exists()
    }

    pub(crate) fn read_project_file(&self, relative_path: &str) -> Result<String> {
        let path = self.project_dir.join(relative_path);
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
    }

    pub(crate) fn remove_project_file(&self, relative_path: &str) -> Result<()> {
        let path = self.project_dir.join(relative_path);
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))
    }

    pub(crate) fn assert_file_contents(&self, relative_path: &str, expected: &str) -> Result<()> {
        let actual = self.read_project_file(relative_path)?;
        if actual != expected {
            let path = self.project_dir.join(relative_path);
            bail!(
                "{} had unexpected contents\nexpected: {:?}\nactual: {:?}",
                path.display(),
                expected,
                actual
            );
        }
        Ok(())
    }

    pub(crate) async fn run_vx_build(&mut self, options: BuildOptions) -> Result<CommandOutput> {
        let work_dir = match &options.subdir {
            Some(subdir) => self.project_dir.join(subdir),
            None => self.project_dir.clone(),
        };
        let label = self.next_command_label("build");
        self.run_vx_command(&label, &work_dir, |cmd| {
            cmd.arg("build");
            if options.release {
                cmd.arg("--release");
            }
            if options.json {
                cmd.arg("--json");
            }
        })
        .await
    }

    pub(crate) async fn run_vx_clean(&mut self, cache: bool) -> Result<CommandOutput> {
        let label = if cache {
            self.next_command_label("clean-cache")
        } else {
            self.next_command_label("clean")
        };
        let work_dir = self.project_dir.clone();
        self.run_vx_command(&label, &work_dir, |cmd| {
            cmd.arg("clean");
            if cache {
                cmd.arg("--cache");
            }
        })
        .await
    }

    fn write_file(&self, base: &Path, relative_path: &str, contents: &str) -> Result<()> {
        let path = base.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    async fn run_vx_command(
        &self,
        label: &str,
        work_dir: &Path,
        configure: impl FnOnce(&mut Command),
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new(&self.shared.vx_binary);
        cmd.current_dir(work_dir);
        cmd.env("PATH", &self.path_env);
        cmd.env("VX_HOME", &self.shared.vx_home);
        cmd.env("VX_HTTP_CACHE_DIR", &self.shared.http_cache_dir);
        cmd.env("VX_TEST", "1");
        cmd.env("VX_TEST_SESSION", &self.session_id);
        cmd.env("VXD", &self.shared.vxd);
        configure(&mut cmd);
        self.run_logged_command(label, &mut cmd, COMMAND_TIMEOUT)
            .await
    }

    async fn run_logged_command(
        &self,
        label: &str,
        cmd: &mut Command,
        limit: Duration,
    ) -> Result<CommandOutput> {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {label} command"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| eyre!("failed to capture stdout for {label}"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| eyre!("failed to capture stderr for {label}"))?;

        let output_task = tokio::spawn(capture_output(stdout, stderr, !self.shared.capture_output));

        let timed_out = match timeout(limit, child.wait()).await {
            Ok(status) => {
                let _ = status.context("failed waiting for child process")?;
                false
            }
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                true
            }
        };

        let captured = output_task
            .await
            .map_err(|error| eyre!("failed to join output capture for {label}: {error}"))??;
        let (stdout_path, stderr_path) =
            self.write_command_logs(label, &captured.stdout, &captured.stderr)?;

        if timed_out {
            bail!(
                "{label} timed out after {:?}\nstdout log: {}\nstderr log: {}\n--- output ---\n{}",
                limit,
                stdout_path.display(),
                stderr_path.display(),
                captured.merged,
            );
        }

        let status = child
            .try_wait()
            .context("failed to poll child process after exit")?
            .ok_or_else(|| eyre!("child exited without a status for {label}"))?;

        Ok(CommandOutput {
            status,
            stdout: captured.stdout,
            stderr: captured.stderr,
            stdout_path,
            stderr_path,
            merged_output: captured.merged,
        })
    }

    fn write_command_logs(
        &self,
        label: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<(PathBuf, PathBuf)> {
        let stdout_path = self.logs_dir.join(format!("{label}.stdout.log"));
        let stderr_path = self.logs_dir.join(format!("{label}.stderr.log"));

        fs::write(&stdout_path, stdout)
            .with_context(|| format!("failed to write {}", stdout_path.display()))?;
        fs::write(&stderr_path, stderr)
            .with_context(|| format!("failed to write {}", stderr_path.display()))?;

        Ok((stdout_path, stderr_path))
    }

    fn next_command_label(&mut self, label: &str) -> String {
        self.command_seq += 1;
        format!("{:03}-{label}", self.command_seq)
    }
}

impl CommandOutput {
    pub(crate) fn was_cached(&self) -> bool {
        self.stdout.contains("Cached (no rebuild needed)")
    }

    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.stdout.contains(needle) || self.stderr.contains(needle)
    }

    pub(crate) fn assert_success(&self, label: &str) -> Result<()> {
        if self.status.success() {
            return Ok(());
        }

        bail!(
            "{label} failed with {status}\nstdout log: {}\nstderr log: {}\n--- output ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.merged_output,
            status = self.status,
        );
    }

    pub(crate) fn assert_cached(&self) -> Result<()> {
        self.assert_success("vx build")?;
        if self.was_cached() {
            return Ok(());
        }

        bail!(
            "expected cache hit but build was not cached\nstdout log: {}\nstderr log: {}\n--- output ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.merged_output,
        );
    }

    pub(crate) fn assert_not_cached(&self) -> Result<()> {
        self.assert_success("vx build")?;
        if !self.was_cached() {
            return Ok(());
        }

        bail!(
            "expected cache miss but build was cached\nstdout log: {}\nstderr log: {}\n--- output ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.merged_output,
        );
    }

    pub(crate) fn assert_failure(&self, label: &str) -> Result<()> {
        if !self.status.success() {
            return Ok(());
        }

        bail!(
            "expected {label} to fail but it succeeded\nstdout log: {}\nstderr log: {}\n--- output ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.merged_output,
        );
    }

    pub(crate) fn assert_failure_contains(&self, label: &str, needle: &str) -> Result<()> {
        self.assert_failure(label)?;
        if self.contains(needle) {
            return Ok(());
        }

        bail!(
            "expected {label} failure to contain {needle:?}\nstdout log: {}\nstderr log: {}\n--- output ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.merged_output,
        );
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }

        let _ = std::process::Command::new(&self.vx_binary)
            .env("VX_HOME", &self.vx_home)
            .env("VX_TEST", "1")
            .arg("kill")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.start_kill();
        }
    }
}

impl DaemonGuard {
    async fn stop(&mut self) -> Result<ShutdownReport> {
        let kill_started = Instant::now();
        let status = std::process::Command::new(&self.vx_binary)
            .env("VX_HOME", &self.vx_home)
            .env("VX_TEST", "1")
            .arg("kill")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("failed to run vx kill")?;
        if !status.success() {
            bail!("vx kill exited with {status}");
        }
        let kill = kill_started.elapsed();

        let wait_started = Instant::now();
        let mut forced = false;
        match timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(status) => {
                let _ = status.context("failed waiting for vxd to exit")?;
            }
            Err(_) => {
                forced = true;
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
            }
        }
        let wait = wait_started.elapsed();
        self.stopped = true;

        Ok(ShutdownReport {
            kill,
            wait,
            forced,
            grace: Duration::ZERO,
        })
    }
}

struct CapturedOutput {
    stdout: String,
    stderr: String,
    merged: String,
}

async fn capture_output(
    stdout: ChildStdout,
    stderr: ChildStderr,
    stream_live: bool,
) -> Result<CapturedOutput> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stdout_task = tokio::spawn(capture_stream(stdout, StreamKind::Stdout, tx.clone()));
    let stderr_task = tokio::spawn(capture_stream(stderr, StreamKind::Stderr, tx));

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    let mut merged = String::new();

    while let Some(item) = rx.recv().await {
        match item.stream {
            StreamKind::Stdout => {
                stdout_buf.push_str(&item.line);
                stdout_buf.push('\n');
                if stream_live {
                    println!("{}", item.line);
                }
                merged.push_str(&format!("{} {}\n", "stdout".cyan().bold(), item.line));
            }
            StreamKind::Stderr => {
                stderr_buf.push_str(&item.line);
                stderr_buf.push('\n');
                if stream_live {
                    eprintln!("{}", item.line);
                }
                merged.push_str(&format!("{} {}\n", "stderr".red().bold(), item.line));
            }
        }
    }

    stdout_task
        .await
        .map_err(|error| eyre!("failed to join stdout capture task: {error}"))??;
    stderr_task
        .await
        .map_err(|error| eyre!("failed to join stderr capture task: {error}"))??;

    Ok(CapturedOutput {
        stdout: stdout_buf,
        stderr: stderr_buf,
        merged,
    })
}

async fn capture_stream<T>(
    stream: T,
    kind: StreamKind,
    tx: mpsc::UnboundedSender<StreamItem>,
) -> Result<()>
where
    T: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        let _ = tx.send(StreamItem { stream: kind, line });
    }
    Ok(())
}

fn ensure_workspace_binaries(workspace_root: &Path) -> Result<()> {
    let debug_dir = target_root(workspace_root).join("debug");
    let missing: Vec<&str> = REQUIRED_BINARIES
        .iter()
        .copied()
        .filter(|name| !debug_dir.join(binary_name(name)).exists())
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    println!("Building required binaries: {}", missing.join(", "));

    let mut cmd = std::process::Command::new("cargo");
    cmd.current_dir(workspace_root);
    cmd.arg("build");
    for package in &missing {
        cmd.arg("--package");
        cmd.arg(package);
    }

    if env::var("VX_DIAGNOSTICS").as_deref() == Ok("1") {
        for package in &missing {
            cmd.arg("--features");
            cmd.arg(format!("{package}/diagnostics"));
        }
    }

    let status = cmd.status().context("failed to run cargo build")?;
    if !status.success() {
        bail!("cargo build failed with {status}");
    }

    Ok(())
}

async fn run_status_with_timeout(
    cmd: &mut Command,
    limit: Duration,
    log_path: &Path,
    label: &str,
) -> Result<ExitStatus> {
    let stdout = File::create(log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;
    let stderr = OpenOptions::new()
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to reopen {}", log_path.display()))?;
    cmd.stdout(Stdio::from(stdout));
    cmd.stderr(Stdio::from(stderr));
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {label}"))?;

    match timeout(limit, child.wait()).await {
        Ok(status) => Ok(status.context("failed waiting for child process")?),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            bail!(
                "{label} timed out after {:?}\nlog: {}\n--- log ---\n{}",
                limit,
                log_path.display(),
                read_log(log_path)
            );
        }
    }
}

fn sanitized_scenario_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect()
}

fn tempdir_scenario_slug(name: &str) -> String {
    sanitized_scenario_name(name).chars().take(24).collect()
}

fn binary_name(name: &str) -> String {
    format!("{name}{}", env::consts::EXE_SUFFIX)
}

fn endpoint_name(name: &str) -> String {
    format!("{name}.sock")
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|_| format!("<unreadable: {}>", path.display()))
}

pub(crate) fn target_root(workspace_root: &Path) -> PathBuf {
    match env::var_os("CARGO_TARGET_DIR") {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                workspace_root.join(path)
            }
        }
        None => workspace_root.join("target"),
    }
}

pub(crate) fn keep_artifacts_env() -> bool {
    env::var("VX_KEEP_INTEGRATION_TMP").as_deref() == Ok("1")
}

fn http_cache_dir() -> PathBuf {
    if let Some(path) = env::var_os("VX_HTTP_CACHE_DIR") {
        return PathBuf::from(path);
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("vx-test-http-cache");
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(cache_home).join("vx-test-http-cache");
        }
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".cache")
                .join("vx-test-http-cache");
        }
    }

    env::temp_dir().join("vx-test-http-cache")
}

fn run_temp_root() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp/vxi-run")
    }

    #[cfg(not(unix))]
    {
        env::temp_dir().join("vx-integration-run")
    }
}

fn scenario_temp_root() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp/vxi")
    }

    #[cfg(not(unix))]
    {
        env::temp_dir().join("vx-integration")
    }
}
