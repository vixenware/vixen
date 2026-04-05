use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use vox::{
    ClientLogging, ClientLoggingOptions, ConnectionAcceptor, ConnectionRequest, Metadata,
    PendingConnection, acceptor_on,
};
use vx_services::VfsClient;

use super::log_event;
use super::logging::real_home_dir;
use super::support::{APP_GROUP_ID, SOCKET_NAME, json_string, runtime};

static ROOT_PEER_STATE: OnceLock<RootPeerState> = OnceLock::new();

#[derive(Clone)]
pub(super) struct RootPeerState {
    app_group_root: PathBuf,
    socket_path: PathBuf,
    inner: Arc<Mutex<RootPeerSnapshot>>,
}

struct RootPeerSnapshot {
    client: Option<VfsClient>,
    connected: bool,
    ever_connected: bool,
    generation: u64,
    disconnect_reason: Option<String>,
}

impl RootPeerState {
    fn new() -> Self {
        let app_group_root = real_home_dir()
            .join("Library")
            .join("Group Containers")
            .join(APP_GROUP_ID);
        let socket_path = app_group_root.join(SOCKET_NAME);
        let _ = std::fs::create_dir_all(&app_group_root);
        let state = Self {
            app_group_root,
            socket_path,
            inner: Arc::new(Mutex::new(RootPeerSnapshot {
                client: None,
                connected: false,
                ever_connected: false,
                generation: 0,
                disconnect_reason: Some("not connected".to_owned()),
            })),
        };
        state.start_listener();
        state
    }

    fn start_listener(&self) {
        let state = self.clone();
        runtime().spawn(async move {
            state.run_listener().await;
        });
    }

    async fn run_listener(self) {
        let socket_path = self.socket_path_string();
        let _ = std::fs::create_dir_all(&self.app_group_root);
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

    fn record_connected(&self, client: VfsClient) -> u64 {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot.generation += 1;
        snapshot.client = Some(client);
        snapshot.connected = true;
        snapshot.ever_connected = true;
        snapshot.disconnect_reason = None;
        snapshot.generation
    }

    fn record_closed(&self, generation: u64, reason: &str) {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        if snapshot.generation != generation {
            return;
        }
        snapshot.client = None;
        snapshot.connected = false;
        snapshot.disconnect_reason = Some(reason.to_owned());
    }

    fn record_disconnect(&self, reason: String) {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot.client = None;
        snapshot.connected = false;
        snapshot.disconnect_reason = Some(reason);
    }

    pub(super) fn connected_client(&self) -> Result<(VfsClient, u64), i32> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        match snapshot.client.clone() {
            Some(client) => Ok((client, snapshot.generation)),
            None => Err(libc::ENOTCONN),
        }
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
        let app_group_path = json_string(self.app_group_root.display().to_string());
        let disconnect_reason = snapshot
            .disconnect_reason
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned());
        format!(
            concat!(
                "{{\n",
                "  \"connected\": {},\n",
                "  \"ever_connected\": {},\n",
                "  \"app_group_path\": {},\n",
                "  \"connected_at\": null,\n",
                "  \"disconnected_at\": null,\n",
                "  \"uptime_seconds\": null,\n",
                "  \"disconnect_reason\": {}\n",
                "}}\n"
            ),
            snapshot.connected, snapshot.ever_connected, app_group_path, disconnect_reason
        )
        .into_bytes()
    }
}

#[derive(Clone)]
struct RootPeerAcceptor {
    state: RootPeerState,
}

impl ConnectionAcceptor for RootPeerAcceptor {
    fn accept(
        &self,
        request: &ConnectionRequest,
        connection: PendingConnection,
    ) -> Result<(), Metadata<'static>> {
        log_event(
            "info",
            format!("root listener request.service={}", request.service()),
        );
        let client = connection.handle_with_client::<VfsClient>(());
        let client: VfsClient = client.with_middleware(ClientLogging::new(ClientLoggingOptions {
            log_metadata: true,
        }));
        let generation = self.state.record_connected(client.clone());
        log_event(
            "info",
            format!("accepted VFS root peer generation={generation}"),
        );
        let state = self.state.clone();
        tokio::spawn(async move {
            client.caller.closed().await;
            state.record_closed(generation, "caller closed");
        });
        Ok(())
    }
}

pub(super) fn root_peer_state() -> &'static RootPeerState {
    ROOT_PEER_STATE.get_or_init(RootPeerState::new)
}
