use camino::Utf8PathBuf;
use eyre::Result;
use moire::process::Child;
use std::time::Duration;

use crate::net::Endpoint;

/// Result of a successful [`ServiceBuilder::connect`].
pub struct ServiceHandle {
    /// The spawned child process, if any (None in connect-only mode).
    pub child: Option<Child>,
    /// Caller for making RPC calls on the root connection.
    pub caller: roam_core::DriverCaller,
    /// Session handle for opening virtual connections.
    pub session_handle: roam_core::SessionHandle,
}

impl ServiceHandle {
    /// Run the boot protocol and open a virtual connection.
    ///
    /// 1. Calls `boot()` — waits for the daemon to initialize
    /// 2. Opens a virtual connection for the real service
    ///
    /// Returns a caller for the virtual connection.
    pub async fn boot_and_connect(&self) -> Result<roam_core::DriverCaller> {
        crate::boot::boot_and_connect(&self.caller, &self.session_handle).await
    }
}

/// Builder for spawning a vixen service and connecting to it via roam.
///
/// The typical flow is:
/// 1. Create a builder with `ServiceBuilder::new("vx-store", endpoint)`
/// 2. Configure with `.caller_name()`, `.args()`, `.log_path()`, `.timeout()`
/// 3. Optionally call `.connect_only()` to skip spawning
/// 4. Call `.connect()` to spawn (if needed) and wait for the roam handshake
pub struct ServiceBuilder {
    binary_name: String,
    endpoint: Endpoint,
    caller_name: Option<String>,
    cli_args: Vec<String>,
    env_vars: Vec<(String, String)>,
    log_path: Option<Utf8PathBuf>,
    timeout: Duration,
    spawn: bool,
}

impl ServiceBuilder {
    /// Start building a service connection.
    ///
    /// - `binary_name`: the binary to spawn (e.g. "vx-store"), resolved as a sibling of the
    ///   current executable, falling back to PATH.
    /// - `endpoint`: where to connect. When spawning, `--listen <endpoint>` is prepended to args.
    pub fn new(binary_name: impl Into<String>, endpoint: Endpoint) -> Self {
        Self {
            binary_name: binary_name.into(),
            endpoint,
            caller_name: None,
            cli_args: Vec::new(),
            env_vars: Vec::new(),
            log_path: None,
            timeout: Duration::from_secs(5),
            spawn: true,
        }
    }

    /// Name used for roam diagnostics (e.g. "vxd", "vx-runner").
    pub fn caller_name(mut self, name: impl Into<String>) -> Self {
        self.caller_name = Some(name.into());
        self
    }

    /// Additional CLI arguments passed to the spawned binary (after `--listen`).
    pub fn args(mut self, args: &[&str]) -> Self {
        self.cli_args.extend(args.iter().map(|s| s.to_string()));
        self
    }

    /// Set an environment variable on the spawned process.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.push((key.into(), value.into()));
        self
    }

    /// Log file path for stdout/stderr when not in test mode.
    ///
    /// In test mode (`VX_TEST` is set), stdout/stderr are always inherited
    /// regardless of this setting.
    pub fn log_path(mut self, path: impl Into<Utf8PathBuf>) -> Self {
        self.log_path = Some(path.into());
        self
    }

    /// Connection timeout (default: 5s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Connect to an already-running service instead of spawning one.
    /// Fails if the service is unreachable within the configured timeout.
    pub fn connect_only(mut self) -> Self {
        self.spawn = false;
        self
    }

    /// Spawn the service (unless connect-only) and return a connected client.
    ///
    /// When spawning, races connection attempts (with exponential backoff)
    /// against the child process exiting. If the child dies before we connect,
    /// we fail immediately instead of retrying until timeout.
    ///
    /// When connect-only, retries with backoff until timeout.
    pub async fn connect(self) -> Result<ServiceHandle> {
        let Self {
            binary_name,
            endpoint,
            caller_name,
            cli_args,
            env_vars,
            log_path,
            timeout,
            spawn,
        } = self;

        let mut child = if spawn {
            tracing::info!("Spawning {binary_name}");
            Some(
                spawn_binary(
                    &binary_name,
                    &endpoint,
                    &cli_args,
                    &env_vars,
                    log_path.as_deref(),
                )
                .await?,
            )
        } else {
            tracing::info!("Connecting to {binary_name} at {endpoint}");
            None
        };

        let connect_fut = connect_with_backoff(&binary_name, &endpoint, &caller_name, timeout);

        if let Some(ref mut child_proc) = child {
            // Race: connect loop vs child dying.
            tokio::select! {
                result = connect_fut => {
                    let (caller, session_handle) = result?;
                    Ok(ServiceHandle { child, caller, session_handle })
                }
                exit_status = child_proc.wait() => {
                    match exit_status {
                        Ok(status) if status.success() => {
                            // Child exited with status 0 — probably found lock held.
                            // Another instance is likely running, so try to connect.
                            tracing::debug!("{binary_name} exited cleanly (lock held?), trying to connect to existing instance");
                            let (caller, session_handle) = connect_with_backoff(&binary_name, &endpoint, &caller_name, timeout).await?;
                            Ok(ServiceHandle { child: None, caller, session_handle })
                        }
                        Ok(status) => eyre::bail!(
                            "{binary_name} exited with {status} before becoming ready at {endpoint}",
                        ),
                        Err(e) => eyre::bail!(
                            "{binary_name} wait failed: {e}",
                        ),
                    }
                }
            }
        } else {
            let (caller, session_handle) = connect_fut.await?;
            Ok(ServiceHandle {
                child,
                caller,
                session_handle,
            })
        }
    }
}

/// Retry connection with exponential backoff until timeout.
async fn connect_with_backoff(
    binary_name: &str,
    endpoint: &Endpoint,
    _caller_name: &Option<String>,
    timeout: Duration,
) -> Result<(roam_core::DriverCaller, roam_core::SessionHandle)> {
    let deadline = tokio::time::Instant::now() + timeout;
    let backoff_ms = [10, 25, 50, 100, 250, 500, 1000];
    let mut last_err = None;

    for delay in backoff_ms.iter().copied().chain(std::iter::repeat(1000)) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(
            remaining.min(Duration::from_secs(1)),
            crate::net::connect_session(endpoint),
        )
        .await
        {
            Ok(Ok((caller, session_handle))) => {
                tracing::debug!("{binary_name} is ready at {endpoint}");
                return Ok((caller, session_handle));
            }
            Ok(Err(e)) => {
                tracing::trace!("{binary_name} not ready yet at {endpoint}: {e}");
                last_err = Some(e.to_string());
            }
            Err(_) => {
                tracing::trace!("{binary_name} connect attempt timed out at {endpoint}");
                last_err = Some("connection timed out".into());
            }
        }

        if tokio::time::Instant::now() + Duration::from_millis(delay) > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    let err_detail = last_err.unwrap_or_else(|| "unknown".into());
    eyre::bail!("{binary_name} unreachable after {timeout:?} at {endpoint}: {err_detail}",);
}

/// Spawn a sibling binary with `--listen <endpoint>` prepended to args.
///
/// - In test mode (`VX_TEST` set): stdout/stderr inherited.
/// - Otherwise: stdout/stderr redirected to `log_path` (or null if not set).
/// - The child dies when the parent dies (via `ur_taking_me_with_you`).
async fn spawn_binary(
    binary_name: &str,
    endpoint: &Endpoint,
    extra_args: &[String],
    env_vars: &[(String, String)],
    log_path: Option<&camino::Utf8Path>,
) -> Result<Child> {
    use std::process::Stdio;

    let vx_test = std::env::var("VX_TEST").is_ok();

    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(binary_name)))
        .filter(|candidate| candidate.exists())
        .unwrap_or_else(|| std::path::PathBuf::from(binary_name));

    let endpoint_display = endpoint.display();

    tracing::debug!(
        %vx_test,
        ?extra_args,
        "spawning {binary_name} from {}",
        binary_path.display()
    );

    let mut cmd = moire::process::Command::new(&binary_path);
    cmd.arg("--listen").arg(&endpoint_display);
    cmd.args(extra_args);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    if vx_test {
        // We do not inherit standard output because it can corrupt `--json` output
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::inherit());
    } else if let Some(log_path) = log_path {
        if let Some(parent) = log_path.parent() {
            moire::fs::create_dir_all(parent).await?;
        }

        let mut opts = moire::fs::OpenOptions::new();
        opts.create(true).append(true);
        let log_file = opts.open(log_path).await?;
        let log_file_clone = log_file.try_clone().await?;

        let stdout_file = log_file_clone.into_std().await;
        let stderr_file = log_file.into_std().await;
        cmd.stdout(Stdio::from(stdout_file));
        cmd.stderr(Stdio::from(stderr_file));
    } else {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    }

    let (inner, diag) = cmd.into_inner_with_diagnostics();
    ur_taking_me_with_you::spawn_dying_with_parent_async(inner)
        .map(|child| Child::from_tokio_with_diagnostics(child, diag))
        .map_err(Into::into)
}
