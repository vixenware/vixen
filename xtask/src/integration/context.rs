use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use eyre::{Context, Result, bail, eyre};
use tempfile::{Builder, TempDir};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep, timeout};

use crate::find_workspace_root;

const REQUIRED_BINARIES: &[&str] = &["vx", "vxd", "vx-runner", "vx-store", "vx-vfsd"];
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct IntegrationContext {
    tempdir: Option<TempDir>,
    root_dir: PathBuf,
    project_dir: PathBuf,
    fake_bin_dir: PathBuf,
    logs_dir: PathBuf,
    vx_home: PathBuf,
    vx_binary: PathBuf,
    path_env: OsString,
    capture_output: bool,
    command_seq: usize,
    daemon: Option<DaemonGuard>,
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
}

struct DaemonGuard {
    child: Child,
    log_path: PathBuf,
    vx_binary: PathBuf,
    vx_home: PathBuf,
    vxd: PathBuf,
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

impl IntegrationContext {
    pub(crate) fn new(name: &str, capture_output: bool) -> Result<Self> {
        let workspace_root =
            find_workspace_root().ok_or_else(|| eyre!("Could not find workspace root"))?;
        let target_dir_root = scenario_temp_root();
        fs::create_dir_all(&target_dir_root)
            .with_context(|| format!("failed to create {}", target_dir_root.display()))?;

        let tempdir = Builder::new()
            .prefix(&format!("vxi-{}.", tempdir_scenario_slug(name)))
            .tempdir_in(&target_dir_root)
            .with_context(|| {
                format!("failed to create temp dir in {}", target_dir_root.display())
            })?;

        ensure_workspace_binaries(&workspace_root)?;

        let root_dir = tempdir.path().to_path_buf();
        let project_dir = root_dir.join("project");
        let fake_bin_dir = root_dir.join("fake-bin");
        let logs_dir = root_dir.join("logs");
        let vx_home = root_dir.join("vx-home");

        for dir in [&project_dir, &fake_bin_dir, &logs_dir, &vx_home] {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }

        let target_dir = target_root(&workspace_root).join("debug");
        let vx_binary = target_dir.join(binary_name("vx"));

        if !vx_binary.exists() {
            bail!("expected vx binary at {}", vx_binary.display());
        }

        let path_env = joined_path(&fake_bin_dir)?;

        Ok(Self {
            tempdir: Some(tempdir),
            root_dir,
            project_dir,
            fake_bin_dir,
            logs_dir,
            vx_home,
            vx_binary,
            path_env,
            capture_output,
            command_seq: 0,
            daemon: None,
        })
    }

    pub(crate) fn keep(&mut self) -> Result<PathBuf> {
        if let Some(tempdir) = self.tempdir.take() {
            return Ok(tempdir.keep());
        }
        Ok(self.root_dir.clone())
    }

    pub(crate) fn write_project_file(&self, relative_path: &str, contents: &str) -> Result<()> {
        self.write_file(&self.project_dir, relative_path, contents)
    }

    pub(crate) fn project_path(&self, relative_path: &str) -> PathBuf {
        self.project_dir.join(relative_path)
    }

    pub(crate) fn write_fake_tool(&self, name: &str, contents: &str) -> Result<()> {
        let path = self.fake_bin_dir.join(binary_name(name));
        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms)
                .with_context(|| format!("failed to chmod {}", path.display()))?;
        }
        Ok(())
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

    pub(crate) async fn ensure_daemon(&mut self) -> Result<()> {
        if self.daemon.is_some() {
            return Ok(());
        }

        let mut daemon = self.start_daemon().await?;
        self.wait_for_daemon(&mut daemon).await?;
        self.daemon = Some(daemon);
        Ok(())
    }

    pub(crate) async fn run_vx_build(&mut self, options: BuildOptions) -> Result<CommandOutput> {
        self.ensure_daemon().await?;
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
        self.ensure_daemon().await?;
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

    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        if let Some(mut daemon) = self.daemon.take() {
            daemon.stop().await?;
            sleep(Duration::from_millis(500)).await;
        }
        Ok(())
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

    async fn start_daemon(&self) -> Result<DaemonGuard> {
        let log_path = self.logs_dir.join("vxd.log");
        let stdout = File::create(&log_path)
            .with_context(|| format!("failed to create {}", log_path.display()))?;
        let stderr = OpenOptions::new()
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to reopen {}", log_path.display()))?;

        let mut cmd = Command::new(&self.vx_binary);
        cmd.current_dir(&self.project_dir);
        cmd.env("PATH", &self.path_env);
        cmd.env("VX_HOME", &self.vx_home);
        cmd.env("VX_TEST", "1");
        cmd.env_remove("VXD");
        cmd.arg("start");
        cmd.arg("--no-tui");
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));

        let child = cmd.spawn().context("failed to start vxd")?;

        Ok(DaemonGuard {
            child,
            log_path,
            vx_binary: self.vx_binary.clone(),
            vx_home: self.vx_home.clone(),
            vxd: self.vx_home.join(endpoint_name("vxd")),
        })
    }

    async fn wait_for_daemon(&self, daemon: &mut DaemonGuard) -> Result<()> {
        let ping_log = self.logs_dir.join("ping.log");
        let deadline = Instant::now() + READINESS_TIMEOUT;

        while Instant::now() < deadline {
            if let Some(status) = daemon.child.try_wait().context("failed to poll vxd")? {
                bail!(
                    "vxd exited with {status} before becoming ready at {}\n--- daemon log ({}) ---\n{}",
                    daemon.vxd.display(),
                    daemon.log_path.display(),
                    read_log(&daemon.log_path)
                );
            }

            let mut cmd = Command::new(&self.vx_binary);
            cmd.current_dir(&self.project_dir);
            cmd.env("PATH", &self.path_env);
            cmd.env("VX_HOME", &self.vx_home);
            cmd.env("VX_TEST", "1");
            cmd.env("VXD", &daemon.vxd);
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
        );
    }

    async fn run_vx_command(
        &self,
        label: &str,
        work_dir: &Path,
        configure: impl FnOnce(&mut Command),
    ) -> Result<CommandOutput> {
        let mut cmd = Command::new(&self.vx_binary);
        cmd.current_dir(work_dir);
        cmd.env("PATH", &self.path_env);
        cmd.env("VX_HOME", &self.vx_home);
        cmd.env("VX_TEST", "1");
        if let Some(daemon) = &self.daemon {
            cmd.env("VXD", &daemon.vxd);
        }
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

        let stdout_task = tokio::spawn(read_child_output(stdout));
        let stderr_task = tokio::spawn(read_child_output(stderr));

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

        let stdout = join_output_task(stdout_task, label, "stdout").await?;
        let stderr = join_output_task(stderr_task, label, "stderr").await?;
        let (stdout_path, stderr_path) = self.write_command_logs(label, &stdout, &stderr)?;

        if !self.capture_output {
            if !stdout.is_empty() {
                print!("{stdout}");
            }
            if !stderr.is_empty() {
                eprint!("{stderr}");
            }
        }

        if timed_out {
            bail!(
                "{label} timed out after {:?}\nstdout log: {}\nstderr log: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                limit,
                stdout_path.display(),
                stderr_path.display(),
                stdout,
                stderr,
            );
        }

        let status = child
            .try_wait()
            .context("failed to poll child process after exit")?
            .ok_or_else(|| eyre!("child exited without a status for {label}"))?;

        Ok(CommandOutput {
            status,
            stdout,
            stderr,
            stdout_path,
            stderr_path,
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
            "{label} failed with {status}\nstdout log: {stdout}\nstderr log: {stderr}\n--- stdout ---\n{stdout_contents}\n--- stderr ---\n{stderr_contents}",
            status = self.status,
            stdout = self.stdout_path.display(),
            stderr = self.stderr_path.display(),
            stdout_contents = self.stdout,
            stderr_contents = self.stderr,
        );
    }

    pub(crate) fn assert_cached(&self) -> Result<()> {
        self.assert_success("vx build")?;
        if self.was_cached() {
            return Ok(());
        }

        bail!(
            "expected cache hit but build was not cached\nstdout log: {}\nstderr log: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.stdout,
            self.stderr,
        );
    }

    pub(crate) fn assert_not_cached(&self) -> Result<()> {
        self.assert_success("vx build")?;
        if !self.was_cached() {
            return Ok(());
        }

        bail!(
            "expected cache miss but build was cached\nstdout log: {}\nstderr log: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.stdout,
            self.stderr,
        );
    }

    pub(crate) fn assert_failure(&self, label: &str) -> Result<()> {
        if !self.status.success() {
            return Ok(());
        }

        bail!(
            "expected {label} to fail but it succeeded\nstdout log: {}\nstderr log: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.stdout,
            self.stderr,
        );
    }

    pub(crate) fn assert_failure_contains(&self, label: &str, needle: &str) -> Result<()> {
        self.assert_failure(label)?;
        if self.contains(needle) {
            return Ok(());
        }

        bail!(
            "expected {label} failure to contain {needle:?}\nstdout log: {}\nstderr log: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout_path.display(),
            self.stderr_path.display(),
            self.stdout,
            self.stderr,
        );
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
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
    async fn stop(&mut self) -> Result<()> {
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

        match timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(status) => {
                let _ = status.context("failed waiting for vxd to exit")?;
            }
            Err(_) => {
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
            }
        }

        Ok(())
    }
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

async fn read_child_output<T>(mut stream: T) -> Result<String>
where
    T: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn join_output_task(
    task: tokio::task::JoinHandle<Result<String>>,
    label: &str,
    stream: &str,
) -> Result<String> {
    task.await
        .map_err(|error| eyre!("failed to join {stream} capture task for {label}: {error}"))?
        .with_context(|| format!("failed reading {stream} for {label}"))
}

fn joined_path(prefix: &Path) -> Result<OsString> {
    let mut paths = vec![prefix.to_path_buf()];
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths).context("failed to construct PATH")
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

pub(crate) fn keep_artifacts() -> bool {
    env::var("VX_KEEP_INTEGRATION_TMP").as_deref() == Ok("1")
}

fn scenario_temp_root() -> PathBuf {
    #[cfg(unix)]
    {
        return PathBuf::from("/tmp/vxi");
    }

    #[cfg(not(unix))]
    {
        env::temp_dir().join("vx-integration")
    }
}
