use camino::Utf8PathBuf;
use eyre::Result;
use std::process::Child;
use std::time::Duration;

use crate::net::{Endpoint, EndpointConnector};

/// Result of a successful [`ServiceBuilder::connect`].
pub struct ServiceHandle<D> {
    /// The spawned child process, if any (None in connect-only mode).
    pub child: Option<Child>,
    /// A connected roam client.
    pub caller: roam_stream::Client<EndpointConnector, D>,
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
    /// When spawning, retries connection with exponential backoff until the
    /// timeout is reached. When connect-only, makes a single attempt.
    pub async fn connect(self) -> Result<ServiceHandle<D>> {
        let child = if self.spawn {
            tracing::info!("Spawning {}", self.binary_name);
            Some(spawn_binary(
                &self.binary_name,
                &self.endpoint,
                &self.cli_args,
                &self.env_vars,
                self.log_path.as_deref(),
            )?)
        } else {
            tracing::info!("Connecting to {} at {}", self.binary_name, self.endpoint);
            None
        };

        if self.spawn {
            // Retry with backoff — the spawned process needs time to start listening.
            let deadline = tokio::time::Instant::now() + self.timeout;
            let backoff_ms = [10, 25, 50, 100, 250, 500, 1000];
            let mut last_err = None;

            for delay in backoff_ms.iter().copied().chain(std::iter::repeat(1000)) {
                // Re-create the client each attempt — a failed handle() poisons the client.
                let attempt_caller = crate::net::reconnecting_client_named(
                    self.endpoint.clone(),
                    self.dispatcher.clone(),
                    self.caller_name.clone(),
                );

                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }

                match tokio::time::timeout(
                    remaining.min(Duration::from_secs(2)),
                    attempt_caller.handle(),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        tracing::info!("{} is ready at {}", self.binary_name, self.endpoint);
                        return Ok(ServiceHandle {
                            child,
                            caller: attempt_caller,
                        });
                    }
                    Ok(Err(e)) => {
                        tracing::debug!(
                            "{} not ready yet at {}: {e}",
                            self.binary_name,
                            self.endpoint,
                        );
                        last_err = Some(e.to_string());
                    }
                    Err(_) => {
                        tracing::debug!(
                            "{} connect attempt timed out at {}",
                            self.binary_name,
                            self.endpoint,
                        );
                        last_err = Some("connection timed out".into());
                    }
                }

                if tokio::time::Instant::now() + Duration::from_millis(delay) > deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }

            let err_detail = last_err.unwrap_or_else(|| "unknown".into());
            eyre::bail!(
                "{} unreachable after {:?} at {}: {}",
                self.binary_name,
                self.timeout,
                self.endpoint,
                err_detail,
            );
        } else {
            // Connect-only: single attempt with timeout.
            let caller = crate::net::reconnecting_client_named(
                self.endpoint.clone(),
                self.dispatcher,
                self.caller_name.clone(),
            );
            match tokio::time::timeout(self.timeout, caller.handle()).await {
                Ok(Ok(_)) => {
                    tracing::info!("{} is ready at {}", self.binary_name, self.endpoint);
                }
                Ok(Err(e)) => {
                    eyre::bail!(
                        "failed to connect to {} at {}: {e}",
                        self.binary_name,
                        self.endpoint,
                    );
                }
                Err(_) => {
                    eyre::bail!(
                        "{} unreachable after {:?} at {}",
                        self.binary_name,
                        self.timeout,
                        self.endpoint,
                    );
                }
            }

            Ok(ServiceHandle { child, caller })
        }
    }
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
        "spawning {} from {}",
        binary_name,
        binary_path.display()
    );

    let mut cmd = std::process::Command::new(&binary_path);
    cmd.arg("--listen").arg(&endpoint_display);
    cmd.args(extra_args);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    if vx_test {
        cmd.stdout(Stdio::inherit());
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

    ur_taking_me_with_you::spawn_dying_with_parent(cmd).map_err(Into::into)
}
