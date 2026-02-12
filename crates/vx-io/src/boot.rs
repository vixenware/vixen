//! Boot protocol for vixen daemons.
//!
//! Every daemon exposes a `Boot` service on the root connection.
//! Clients call `boot()` to initialize the daemon, then open a
//! virtual connection to access the real service.
//!
//! # Server side
//!
//! Use [`BootGate`] to implement the boot protocol. It manages the
//! boot state, provides a [`Boot`] service implementation, and gates
//! virtual connection acceptance until boot completes.
//!
//! # Client side
//!
//! Use [`boot_and_connect`] after establishing a roam connection to
//! call `boot()` and open a virtual connection in one step.

use std::sync::Arc;

use eyre::Result;
use facet::Facet;
use roam_stream::{ConnectionHandle, ServiceDispatcher};
use tokio::sync::watch;

/// Optional cleanup hook for services that need to do work before exit.
pub trait GracefulShutdown {
    /// Called before `process::exit(0)` when shutdown is requested.
    fn graceful_shutdown(&self) -> impl std::future::Future<Output = ()> + Send;
}

impl<T: GracefulShutdown + Send + Sync> GracefulShutdown for Arc<T> {
    async fn graceful_shutdown(&self) {
        T::graceful_shutdown(self).await;
    }
}

/// Result of a boot attempt.
#[derive(Debug, Clone, Facet)]
pub struct BootResult {
    /// Whether boot succeeded.
    pub success: bool,
    /// Error message if boot failed.
    pub error: Option<String>,
}

/// Boot service — exposed on the root connection of every daemon.
///
/// The protocol is:
/// 1. Client connects, gets a root connection with only `Boot`
/// 2. Client calls `boot()` — daemon initializes
/// 3. If boot succeeds, client opens a virtual connection for the real service
/// 4. If boot fails, client gets a structured error and can retry or report
#[roam::service]
pub trait Boot {
    /// Initialize the daemon. Returns success/failure.
    /// The daemon stays alive regardless of outcome.
    async fn boot(&self) -> BootResult;

    /// Shut the daemon down. Always available, even if boot failed.
    async fn shutdown(&self);
}

// ─── Boot state ─────────────────────────────────────────────────────────────

/// Boot state for a daemon's initialization.
#[derive(Clone)]
pub enum BootState {
    /// Daemon is still initializing.
    Booting,
    /// Daemon initialized successfully.
    Ready,
    /// Daemon initialization failed.
    Failed(String),
}

// ─── Server-side boot gate ──────────────────────────────────────────────────

/// Manages the boot lifecycle for a daemon.
///
/// Create with [`BootGate::new`], which returns the gate and a [`BootTx`] sender.
/// The daemon's boot task uses `BootTx` to signal success or failure.
/// The gate provides a [`Boot`] service implementation and gates virtual
/// connection acceptance.
pub struct BootGate<S> {
    boot_rx: watch::Receiver<BootState>,
    service: tokio::sync::OnceCell<S>,
}

/// Sender half — used by the daemon's boot task to signal completion.
pub struct BootTx {
    tx: watch::Sender<BootState>,
}

impl BootTx {
    /// Signal that boot succeeded.
    pub fn ready(self) {
        let _ = self.tx.send(BootState::Ready);
    }

    /// Signal that boot failed.
    pub fn failed(self, msg: String) {
        let _ = self.tx.send(BootState::Failed(msg));
    }
}

impl<S: Send + Sync + 'static> BootGate<S> {
    /// Create a new boot gate and its sender.
    pub fn new() -> (Arc<Self>, BootTx) {
        let (tx, rx) = watch::channel(BootState::Booting);
        let gate = Arc::new(Self {
            boot_rx: rx,
            service: tokio::sync::OnceCell::new(),
        });
        let boot_tx = BootTx { tx };
        (gate, boot_tx)
    }

    /// Store the successfully-initialized service in the gate.
    ///
    /// Called by the boot task after initialization succeeds, before
    /// calling `boot_tx.ready()`.
    pub fn set_service(&self, service: S) {
        let _ = self.service.set(service);
    }

    /// Get the service, if boot has completed.
    pub fn service(&self) -> Option<&S> {
        self.service.get()
    }

    /// Subscribe to boot state changes.
    pub fn subscribe(&self) -> watch::Receiver<BootState> {
        self.boot_rx.clone()
    }

    /// Create a [`BootService`] that implements the [`Boot`] trait.
    pub fn boot_service(self: &Arc<Self>) -> BootService<S> {
        BootService {
            gate: Arc::clone(self),
        }
    }
}

/// Boot service implementation backed by a [`BootGate`].
#[derive(Clone)]
pub struct BootService<S> {
    gate: Arc<BootGate<S>>,
}

impl<S> BootService<S> {
    /// Access the underlying gate (e.g. to check service readiness for virtual connections).
    pub fn gate(&self) -> &Arc<BootGate<S>> {
        &self.gate
    }
}

impl<S: GracefulShutdown + Send + Sync + 'static> Boot for BootService<S> {
    async fn shutdown(&self, _cx: &roam::Context) {
        tracing::info!("Shutdown requested via Boot service");
        if let Some(service) = self.gate.service() {
            service.graceful_shutdown().await;
        }
        std::process::exit(0);
    }

    async fn boot(&self, _cx: &roam::Context) -> BootResult {
        let mut rx = self.gate.boot_rx.clone();
        loop {
            {
                let state = rx.borrow().clone();
                match state {
                    BootState::Booting => { /* wait */ }
                    BootState::Ready => {
                        return BootResult {
                            success: true,
                            error: None,
                        };
                    }
                    BootState::Failed(msg) => {
                        return BootResult {
                            success: false,
                            error: Some(msg),
                        };
                    }
                }
            }
            if rx.changed().await.is_err() {
                return BootResult {
                    success: false,
                    error: Some("boot state channel closed".to_string()),
                };
            }
        }
    }
}

// ─── Client-side boot helper ────────────────────────────────────────────────

/// Call `boot()` on a root connection and open a virtual connection.
///
/// This is the standard client-side boot protocol:
/// 1. Call `boot()` on the root connection
/// 2. If boot fails, return the error
/// 3. Open a virtual connection (optionally with a dispatcher for callbacks)
/// 4. Return the virtual connection handle
pub async fn boot_and_connect(
    root_handle: &ConnectionHandle,
    dispatcher: Option<Box<dyn ServiceDispatcher>>,
) -> Result<ConnectionHandle> {
    let boot_client = BootClient::new(root_handle.clone());
    let boot_result = boot_client.boot().await?;
    if !boot_result.success {
        let msg = boot_result
            .error
            .unwrap_or_else(|| "unknown boot error".into());
        eyre::bail!("boot failed: {msg}");
    }

    let virtual_handle = root_handle
        .connect(vec![], dispatcher)
        .await
        .map_err(|e| eyre::eyre!("failed to open virtual connection: {e}"))?;

    Ok(virtual_handle)
}
