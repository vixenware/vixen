use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use vox::{
    ClientLogging, ClientLoggingOptions, ConnectionRequest, ConnectionRoute, ConnectionRouter,
    Metadata, ReplySink, acceptor_on,
};
use vx_fskit_types::StatusPayload;
use vx_services::VfsClient;

use super::OwnedVixenFsConfig;
use super::log_event;
use super::logging::log_filter_path;
use super::support::runtime;

static ROOT_PEER_STATE: OnceLock<RootPeerState> = OnceLock::new();
const STATUS_JSON_SIZE: usize = 8192;

#[derive(Clone)]
pub(super) struct RootPeerState {
    socket_path: PathBuf,
    bundle_version: String,
    bundle_identifier: String,
    app_group_path: PathBuf,
    log_filter_path: PathBuf,
    inner: Arc<Mutex<RootPeerSnapshot>>,
    /// Set once the VFS socket listener has been started. Only the mounting instance
    /// flips this; probe-only instances never do, so they never bind the socket.
    listener_started: Arc<AtomicBool>,
}

/// One connected prefix provider: the client we call to serve that prefix's subtree, and
/// the connection generation it was registered under (for stale-close detection).
struct PrefixClient {
    client: VfsClient,
    generation: u64,
}

struct RootPeerSnapshot {
    /// Live prefix providers, keyed by the prefix each connection claimed (its
    /// `metadata.prefix`). Each is a virtual vox connection serving one Vfs; the volume
    /// mounts each under `/<prefix>/`. Empty = nothing connected.
    clients: HashMap<String, PrefixClient>,
    ever_connected: bool,
    /// Monotonic connection counter; also the cache-invalidation key for routed items.
    generation: u64,
    disconnect_reason: Option<String>,
}

impl RootPeerState {
    fn new(config: &OwnedVixenFsConfig) -> Self {
        let _ = std::fs::create_dir_all(&config.app_group_path);
        let state = Self {
            socket_path: config.socket_path.clone(),
            bundle_version: config.bundle_version.clone(),
            bundle_identifier: config.bundle_identifier.clone(),
            app_group_path: config.app_group_path.clone(),
            log_filter_path: log_filter_path(&config.app_group_path),
            inner: Arc::new(Mutex::new(RootPeerSnapshot {
                clients: HashMap::new(),
                ever_connected: false,
                generation: 0,
                disconnect_reason: Some("not connected".to_owned()),
            })),
            listener_started: Arc::new(AtomicBool::new(false)),
        };
        // NB: the listener is NOT started here. A single `mount` makes FSKit create the
        // filesystem object in two processes — a probe instance and a load instance — and
        // starting the socket listener at creation made both bind vixenfs.sock, so a
        // backend connecting got ECONNREFUSED. Only the mounting instance starts it, via
        // `ensure_listening()` from `loadResource`.
        state
    }

    /// Start the VFS socket listener exactly once. Called from the mount path
    /// (`loadResource`/`didFinishLoading`); probe-only instances never call it and so
    /// never bind the socket.
    pub(super) fn ensure_listening(&self) {
        if self.listener_started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.start_listener();
    }

    fn start_listener(&self) {
        let state = self.clone();
        runtime().spawn(async move {
            state.run_listener().await;
        });
    }

    async fn run_listener(self) {
        let socket_path = self.socket_path_string();
        let _ = vox::transport::local::remove_endpoint(&socket_path);
        log_event(
            "info",
            format!("starting VFS root listener addr=local://{socket_path}"),
        );
        let listener = match vox::transport::local::LocalLinkAcceptor::bind(&socket_path) {
            Ok(listener) => listener,
            Err(error) => {
                self.record_disconnect(format!("listener_bind_error:{error}"));
                log_event(
                    "error",
                    format!("failed to bind VFS root listener: {error}"),
                );
                return;
            }
        };

        loop {
            log_event("debug", "waiting for VFS root peer");
            match listener.accept().await {
                Ok(link) => {
                    let state = self.clone();
                    tokio::spawn(async move {
                        state.accept_link(link).await;
                    });
                }
                Err(error) => {
                    self.record_disconnect(format!("listener_accept_error:{error}"));
                    log_event("error", format!("VFS root listener accept failed: {error}"));
                    return;
                }
            }
        }
    }

    async fn accept_link(self, link: vox::transport::local::LocalLink) {
        match acceptor_on(link)
            .on_connection(RootPeerAcceptor {
                state: self.clone(),
            })
            .establish::<vox::NoopClient>()
            .await
        {
            Ok(client) => {
                log_event("info", "VFS root peer session established");
                client.caller.closed().await;
            }
            Err(error) => {
                self.record_disconnect(format!("accept_error:{error}"));
                log_event("error", format!("failed to accept VFS root peer: {error}"));
            }
        }
    }

    fn record_connected(&self, prefix: String, client: VfsClient) -> u64 {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot.generation += 1;
        let generation = snapshot.generation;
        snapshot
            .clients
            .insert(prefix, PrefixClient { client, generation });
        snapshot.ever_connected = true;
        snapshot.disconnect_reason = None;
        generation
    }

    fn record_closed(&self, prefix: &str, generation: u64, reason: &str) {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        // Only drop the entry if it's still the one that closed — a newer connection may
        // have re-claimed the same prefix in the meantime.
        if snapshot
            .clients
            .get(prefix)
            .is_some_and(|entry| entry.generation == generation)
        {
            snapshot.clients.remove(prefix);
        }
        if snapshot.clients.is_empty() {
            snapshot.disconnect_reason = Some(reason.to_owned());
        }
    }

    fn record_disconnect(&self, reason: String) {
        // An accept that never registered a client (e.g. handshake failure) — just note why,
        // without disturbing any live prefixes.
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        if snapshot.clients.is_empty() {
            snapshot.disconnect_reason = Some(reason);
        }
    }

    /// The client serving the connection with this generation — used to route a routed item
    /// back to the prefix provider that owns it.
    pub(super) fn client_for_generation(&self, generation: u64) -> Result<VfsClient, i32> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot
            .clients
            .values()
            .find(|entry| entry.generation == generation)
            .map(|entry| entry.client.clone())
            .ok_or(libc::ENOTCONN)
    }

    /// The client for a named prefix, plus its generation (used to enter `/<prefix>/`).
    pub(super) fn client_for_prefix(&self, prefix: &str) -> Option<(VfsClient, u64)> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot
            .clients
            .get(prefix)
            .map(|entry| (entry.client.clone(), entry.generation))
    }

    /// Live prefixes and the generation serving each, sorted by name — the directories the
    /// mount root exposes.
    pub(super) fn live_prefixes(&self) -> Vec<(String, u64)> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        let mut prefixes: Vec<(String, u64)> = snapshot
            .clients
            .iter()
            .map(|(name, entry)| (name.clone(), entry.generation))
            .collect();
        prefixes.sort_by(|a, b| a.0.cmp(&b.0));
        prefixes
    }

    /// All live connection generations — the set a routed-item cache should retain.
    pub(super) fn live_generations(&self) -> Vec<u64> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot
            .clients
            .values()
            .map(|entry| entry.generation)
            .collect()
    }

    pub(super) fn generation(&self) -> u64 {
        self.inner
            .lock()
            .expect("root peer snapshot mutex poisoned")
            .generation
    }

    fn socket_path_string(&self) -> String {
        self.socket_path.display().to_string()
    }

    pub(super) fn status_json_bytes(&self) -> Vec<u8> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");

        let payload = StatusPayload {
            connected: !snapshot.clients.is_empty(),
            ever_connected: snapshot.ever_connected,
            bundle_version: Some(self.bundle_version.clone()),
            bundle_identifier: Some(self.bundle_identifier.clone()),
            app_group_path: Some(self.app_group_path.display().to_string()),
            socket_path: Some(self.socket_path.display().to_string()),
            log_filter_path: Some(self.log_filter_path.display().to_string()),
            extension_pid: Some(unsafe { libc::getpid() as u32 }),
            connected_at: None,
            disconnected_at: None,
            uptime_seconds: None,
            disconnect_reason: snapshot.disconnect_reason.clone(),
        };
        let mut bytes = facet_json::to_string(&payload)
            .expect("StatusPayload serialization should not fail")
            .into_bytes();
        if bytes.len() < STATUS_JSON_SIZE {
            bytes.resize(STATUS_JSON_SIZE - 1, b' ');
            bytes.push(b'\n');
        }
        bytes
    }
}

#[derive(Clone)]
struct RootPeerAcceptor {
    state: RootPeerState,
}

#[derive(Clone, Copy)]
struct PrefixProviderHandler;

impl vox::Handler<vox::DriverReplySink> for PrefixProviderHandler {
    async fn handle(
        &self,
        _call: vox::SelfRef<vox::RequestCall<'static>>,
        reply: vox::DriverReplySink,
        _schemas: Arc<vox::SchemaRecvTracker>,
    ) {
        reply
            .send_error(vox::VoxError::<core::convert::Infallible>::UnknownMethod)
            .await;
    }
}

impl ConnectionRouter for RootPeerAcceptor {
    fn route(&self, request: &ConnectionRequest) -> Result<ConnectionRoute, Metadata> {
        let prefix = request
            .get_str("prefix")
            .filter(|prefix| !prefix.is_empty())
            .map(str::to_owned);
        let service = request.service().to_owned();
        let state = self.state.clone();

        // A provider claims its mount prefix via connection metadata — one virtual vox
        // connection == one prefix == one Vfs (one-service-per-connection). Connections
        // without a prefix (e.g. the peer's root/session anchor) are accepted but register
        // no provider, so they don't tear the session down.
        Ok(ConnectionRoute::handle_with_client::<VfsClient>(
            PrefixProviderHandler,
            move |client| {
                let client: VfsClient =
                    client.with_middleware(ClientLogging::new(ClientLoggingOptions {
                        log_metadata: true,
                    }));
                match prefix {
                    Some(prefix) => {
                        log_event(
                            "info",
                            format!("accepting VFS provider prefix={prefix} service={service}"),
                        );
                        let generation = state.record_connected(prefix.clone(), client.clone());
                        log_event(
                            "info",
                            format!(
                                "accepted VFS provider prefix={prefix} generation={generation}"
                            ),
                        );
                        tokio::spawn(async move {
                            client.caller.closed().await;
                            state.record_closed(&prefix, generation, "caller closed");
                        });
                    }
                    None => {
                        log_event(
                            "info",
                            format!(
                                "accepted connection with no prefix (not a provider) service={service}"
                            ),
                        );
                    }
                }
            },
        ))
    }
}

pub(super) fn init_root_peer_state(config: &OwnedVixenFsConfig) {
    ROOT_PEER_STATE.get_or_init(|| RootPeerState::new(config));
}

pub(super) fn root_peer_state() -> &'static RootPeerState {
    ROOT_PEER_STATE
        .get()
        .expect("root peer state not initialized (init_root_peer_state must be called first)")
}
