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
    capture_output: bool,
}

pub(crate) struct LoggedCommand {
    status: ExitStatus,
    stdout_path: Option<PathBuf>,
    stderr_path: Option<PathBuf>,
}

pub(crate) struct DaemonGuard {
    child: Child,
    log_path: Option<PathBuf>,
    vx_binary: PathBuf,
    vx_home: PathBuf,
    pub(crate) vxd: PathBuf,
}

impl IntegrationContext {
    pub(crate) fn new(name: &str, capture_output: bool) -> Result<Self> {
        let workspace_root =
            find_workspace_root().ok_or_else(|| eyre!("Could not find workspace root"))?;
        let target_dir_root = target_root(&workspace_root);
        fs::create_dir_all(&target_dir_root)
            .with_context(|| format!("failed to create {}", target_dir_root.display()))?;

        let tempdir = Builder::new()
            .prefix(&format!("vx-integration-{name}."))
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

        Ok(Self {
            tempdir: Some(tempdir),
            root_dir,
            project_dir,
            fake_bin_dir,
            logs_dir,
            vx_home,
            vx_binary,
            capture_output,
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

    pub(crate) fn prefixed_path(&self) -> Result<OsString> {
        let mut paths = vec![self.fake_bin_dir.clone()];
        if let Some(existing) = env::var_os("PATH") {
            paths.extend(env::split_paths(&existing));
        }
        env::join_paths(paths).context("failed to construct PATH")
    }

    pub(crate) async fn start_daemon(&self, path_env: &OsString) -> Result<DaemonGuard> {
        let mut cmd = Command::new(&self.vx_binary);
        cmd.current_dir(&self.project_dir);
        cmd.env("PATH", path_env);
        cmd.env("VX_HOME", &self.vx_home);
        cmd.env("VX_TEST", "1");
        cmd.env_remove("VXD");
        cmd.arg("start");
        cmd.arg("--no-tui");

        let log_path = if self.capture_output {
            let log_path = self.logs_dir.join("vxd.log");
            let stdout = File::create(&log_path)
                .with_context(|| format!("failed to create {}", log_path.display()))?;
            let stderr = OpenOptions::new()
                .append(true)
                .open(&log_path)
                .with_context(|| format!("failed to reopen {}", log_path.display()))?;
            cmd.stdout(Stdio::from(stdout));
            cmd.stderr(Stdio::from(stderr));
            Some(log_path)
        } else {
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
            None
        };

        let child = cmd.spawn().context("failed to start vxd")?;

        Ok(DaemonGuard {
            child,
            log_path,
            vx_binary: self.vx_binary.clone(),
            vx_home: self.vx_home.clone(),
            vxd: self.vx_home.join(endpoint_name("vxd")),
        })
    }

    pub(crate) async fn wait_for_daemon(
        &self,
        path_env: &OsString,
        daemon: &DaemonGuard,
    ) -> Result<()> {
        let ping_log = self.logs_dir.join("ping.log");
        let deadline = Instant::now() + READINESS_TIMEOUT;

        while Instant::now() < deadline {
            let mut cmd = Command::new(&self.vx_binary);
            cmd.current_dir(&self.project_dir);
            cmd.env("PATH", path_env);
            cmd.env("VX_HOME", &self.vx_home);
            cmd.env("VX_TEST", "1");
            cmd.env("VXD", &daemon.vxd);
            cmd.arg("ping");

            let status = run_status_with_timeout(
                self.capture_output,
                &mut cmd,
                Duration::from_secs(2),
                &ping_log,
                "vx ping",
            )
            .await?;

            if status.success() {
                return Ok(());
            }

            sleep(Duration::from_millis(100)).await;
        }

        match &daemon.log_path {
            Some(log_path) => bail!(
                "vxd did not become ready within {:?}\n--- daemon log ({}) ---\n{}",
                READINESS_TIMEOUT,
                log_path.display(),
                read_log(log_path)
            ),
            None => bail!("vxd did not become ready within {:?}", READINESS_TIMEOUT),
        }
    }

    pub(crate) async fn run_vx_build(
        &self,
        path_env: &OsString,
        vxd: &Path,
    ) -> Result<LoggedCommand> {
        let mut cmd = Command::new(&self.vx_binary);
        cmd.current_dir(&self.project_dir);
        cmd.env("PATH", path_env);
        cmd.env("VX_HOME", &self.vx_home);
        cmd.env("VX_TEST", "1");
        cmd.env("VXD", vxd);
        cmd.arg("build");

        self.run_logged_command("build", &mut cmd, COMMAND_TIMEOUT)
            .await
    }

    pub(crate) fn assert_file_contents(&self, relative_path: &str, expected: &str) -> Result<()> {
        let path = self.project_dir.join(relative_path);
        let actual = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if actual != expected {
            bail!(
                "{} had unexpected contents\nexpected: {:?}\nactual: {:?}",
                path.display(),
                expected,
                actual
            );
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

    async fn run_logged_command(
        &self,
        label: &str,
        cmd: &mut Command,
        limit: Duration,
    ) -> Result<LoggedCommand> {
        let (stdout_path, stderr_path) = if self.capture_output {
            let stdout_path = self.logs_dir.join(format!("{label}.stdout.log"));
            let stderr_path = self.logs_dir.join(format!("{label}.stderr.log"));

            let stdout = File::create(&stdout_path)
                .with_context(|| format!("failed to create {}", stdout_path.display()))?;
            let stderr = File::create(&stderr_path)
                .with_context(|| format!("failed to create {}", stderr_path.display()))?;

            cmd.stdout(Stdio::from(stdout));
            cmd.stderr(Stdio::from(stderr));
            (Some(stdout_path), Some(stderr_path))
        } else {
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
            (None, None)
        };

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {label} command"))?;

        match timeout(limit, child.wait()).await {
            Ok(status) => Ok(LoggedCommand {
                status: status.context("failed waiting for child process")?,
                stdout_path,
                stderr_path,
            }),
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                if let (Some(stdout_path), Some(stderr_path)) = (&stdout_path, &stderr_path) {
                    bail!(
                        "{label} timed out after {:?}\nstdout log: {}\nstderr log: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        limit,
                        stdout_path.display(),
                        stderr_path.display(),
                        read_log(stdout_path),
                        read_log(stderr_path),
                    );
                }
                bail!("{label} timed out after {:?}", limit);
            }
        }
    }
}

impl LoggedCommand {
    pub(crate) fn assert_success(&self, label: &str) -> Result<()> {
        if self.status.success() {
            return Ok(());
        }

        bail!(
            "{label} failed with {status}\nstdout log: {stdout}\nstderr log: {stderr}\n--- stdout ---\n{stdout_contents}\n--- stderr ---\n{stderr_contents}",
            status = self.status,
            stdout = self
                .stdout_path
                .as_ref()
                .map_or("<not captured>".to_string(), |p| p.display().to_string()),
            stderr = self
                .stderr_path
                .as_ref()
                .map_or("<not captured>".to_string(), |p| p.display().to_string()),
            stdout_contents = self
                .stdout_path
                .as_ref()
                .map_or("<stdout not captured>".to_string(), |p| read_log(p)),
            stderr_contents = self
                .stderr_path
                .as_ref()
                .map_or("<stderr not captured>".to_string(), |p| read_log(p)),
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
    capture_output: bool,
    cmd: &mut Command,
    limit: Duration,
    log_path: &Path,
    label: &str,
) -> Result<ExitStatus> {
    if capture_output {
        let stdout = File::create(log_path)
            .with_context(|| format!("failed to create {}", log_path.display()))?;
        let stderr = OpenOptions::new()
            .append(true)
            .open(log_path)
            .with_context(|| format!("failed to reopen {}", log_path.display()))?;
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));
    } else {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {label}"))?;

    match timeout(limit, child.wait()).await {
        Ok(status) => Ok(status.context("failed waiting for child process")?),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            if capture_output {
                bail!(
                    "{label} timed out after {:?}\nlog: {}\n--- log ---\n{}",
                    limit,
                    log_path.display(),
                    read_log(log_path)
                );
            }
            bail!("{label} timed out after {:?}", limit);
        }
    }
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
