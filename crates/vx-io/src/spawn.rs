use camino::Utf8PathBuf;
use eyre::Result;
use std::time::Duration;
use tokio::process::Child;

use crate::net::{Endpoint, EndpointConnector};

/// Result of a successful [`ServiceBuilder::connect`].
pub struct ServiceHandle<D> {
    /// The spawned child process, if any (None in connect-only mode).
    pub child: Option<Child>,
    /// A connected roam client.
    pub caller: roam_stream::Client<EndpointConnector, D>,
}

impl<D> ServiceHandle<D>
where
    D: roam_stream::ServiceDispatcher + Clone + 'static,
{
    /// Run the boot protocol and open a virtual connection.
    ///
    /// 1. Gets the root connection handle
    /// 2. Calls `boot()` — waits for the daemon to initialize
    /// 3. Opens a virtual connection for the real service
    ///
    /// Returns the virtual connection handle.
    pub async fn boot_and_connect(
        &self,
        dispatcher: Option<Box<dyn roam_stream::ServiceDispatcher>>,
    ) -> Result<roam_stream::ConnectionHandle> {
        let root_handle = self.caller.handle().await?;
        crate::boot::boot_and_connect(&root_handle, dispatcher).await
    }
}

/// Builder for spawning a vixen service and connecting to it via roam.
///
/// The typical flow is:
/// 1. Create a builder with `ServiceBuilder::new("vx-store", endpoint, dispatcher)`
/// 2. Configure with `.caller_name()`, `.args()`, `.log_path()`, `.timeout()`
/// 3. Optionally call `.connect_only()` to skip spawning
/// 4. Call `.connect()` to spawn (if needed) and wait for the roam handshake
pub struct ServiceBuilder<D> {
    binary_name: String,
    endpoint: Endpoint,
    dispatcher: D,
    caller_name: Option<String>,
    cli_args: Vec<String>,
    env_vars: Vec<(String, String)>,
    log_path: Option<Utf8PathBuf>,
    timeout: Duration,
    spawn: bool,
}

impl<D> ServiceBuilder<D>
where
    D: roam_stream::ServiceDispatcher + Clone + 'static,
{
    /// Start building a service connection.
    ///
    /// - `binary_name`: the binary to spawn (e.g. "vx-store"), resolved as a sibling of the
    ///   current executable, falling back to PATH.
    /// - `endpoint`: where to connect. When spawning, `--listen <endpoint>` is prepended to args.
    /// - `dispatcher`: roam dispatcher for incoming RPCs on this connection.
    pub fn new(binary_name: impl Into<String>, endpoint: Endpoint, dispatcher: D) -> Self {
        Self {
            binary_name: binary_name.into(),
            endpoint,
            dispatcher,
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
    pub async fn connect(self) -> Result<ServiceHandle<D>> {
        let Self {
            binary_name,
            endpoint,
            dispatcher,
            caller_name,
            cli_args,
            env_vars,
            log_path,
            timeout,
            spawn,
        } = self;

        let mut child = if spawn {
            tracing::info!("Spawning {binary_name}");
            Some(spawn_binary(
                &binary_name,
                &endpoint,
                &cli_args,
                &env_vars,
                log_path.as_deref(),
            )?)
        } else {
            tracing::info!("Connecting to {binary_name} at {endpoint}");
            None
        };

        let connect_fut =
            connect_with_backoff(&binary_name, &endpoint, &dispatcher, &caller_name, timeout);

        if let Some(ref mut child_proc) = child {
            // Race: connect loop vs child dying.
            tokio::select! {
                result = connect_fut => {
                    let caller = result?;
                    Ok(ServiceHandle { child, caller })
                }
                exit_status = child_proc.wait() => {
                    match exit_status {
                        Ok(status) if status.success() => {
                            // Child exited with status 0 — probably found lock held.
                            // Another instance is likely running, so try to connect.
                            tracing::debug!("{binary_name} exited cleanly (lock held?), trying to connect to existing instance");
                            let caller = connect_with_backoff(&binary_name, &endpoint, &dispatcher, &caller_name, timeout).await?;
                            Ok(ServiceHandle { child: None, caller })
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
            let caller = connect_fut.await?;
            Ok(ServiceHandle { child, caller })
        }
    }
}

/// Retry connection with exponential backoff until timeout.
async fn connect_with_backoff<D>(
    binary_name: &str,
    endpoint: &Endpoint,
    dispatcher: &D,
    caller_name: &Option<String>,
    timeout: Duration,
) -> Result<roam_stream::Client<EndpointConnector, D>>
where
    D: roam_stream::ServiceDispatcher + Clone + 'static,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let backoff_ms = [10, 25, 50, 100, 250, 500, 1000];
    let mut last_err = None;

    for delay in backoff_ms.iter().copied().chain(std::iter::repeat(1000)) {
        let attempt_caller = crate::net::reconnecting_client_named(
            endpoint.clone(),
            dispatcher.clone(),
            caller_name.clone(),
        );

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(
            remaining.min(Duration::from_secs(1)),
            attempt_caller.handle(),
        )
        .await
        {
            Ok(Ok(_)) => {
                tracing::debug!("{binary_name} is ready at {endpoint}");
                return Ok(attempt_caller);
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
fn spawn_binary(
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

    let mut cmd = tokio::process::Command::new(&binary_path);
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
        use std::fs::OpenOptions;

        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;

        cmd.stdout(Stdio::from(log_file.try_clone()?));
        cmd.stderr(Stdio::from(log_file));
    } else {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    }

    ur_taking_me_with_you::spawn_dying_with_parent_async(cmd).map_err(Into::into)
}
