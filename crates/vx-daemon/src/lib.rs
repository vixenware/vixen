//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Can serve the store service, the runner service, the vfs service

use std::{
    fs::{File, remove_file},
    future::Future,
    io,
    path::Path,
    pin::Pin,
    time::Duration,
};

use camino::{Utf8Path, Utf8PathBuf};
use eyre::Result;
use fs4::fs_std::FileExt;
use tokio::{net::TcpListener, time};
use vox::{
    AcceptedConnection, Caller, ConnectionAcceptor, ConnectionId, ConnectionSettings, DriverCaller,
    Metadata, MetadataEntry, MetadataFlags, MetadataValue, SessionHandle, TransportMode,
};
use vox_stream::{LocalLink, LocalLinkAcceptor, StreamLink};
use vx_services::{DaemonClient, DaemonDispatcher};

mod service;
pub use service::DaemonService;

#[cfg(test)]
mod tests;

const DAEMON_HEALTH_TIMEOUT: Duration = Duration::from_millis(500);
const UNIX_LOCK_RECOVERY_TIMEOUT: Duration = Duration::from_millis(500);
const UNIX_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

type TcpLink = StreamLink<tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf>;
type AcceptFuture<'a> =
    Pin<Box<dyn Future<Output = (usize, io::Result<AcceptedLink>)> + Send + 'a>>;

pub type UpstreamCaller = DriverCaller;

pub enum ListenAddr {
    /// e.g. `/tmp/vx-daemon.sock`
    UnixSocket(Utf8PathBuf),

    /// e.g. `0.0.0.0:9977`
    Tcp(std::net::SocketAddr),

    /// TODO: windows named pipe
    NamedPipe(()),
}

pub struct DaemonConfig {
    /// All addresses to listen on
    pub listen: Vec<ListenAddr>,
}

struct ListenerSet {
    listeners: Vec<BoundListener>,
}

enum BoundListener {
    Unix(UnixSocketListener),
    Tcp(TcpSocketListener),
}

struct UnixSocketListener {
    path: Utf8PathBuf,
    listener: LocalLinkAcceptor,
    _lock: File,
}

struct TcpSocketListener {
    addr: std::net::SocketAddr,
    listener: TcpListener,
}

enum AcceptedLink {
    Local(LocalLink),
    Tcp(TcpLink),
}

/// Serve the VXD service on all configured listen addresses.
pub async fn serve(config: DaemonConfig) -> Result<()> {
    if config.listen.is_empty() {
        return Err(eyre::eyre!("no listen addresses configured"));
    }

    let listeners = ListenerSet::bind(config.listen).await?;

    loop {
        let link = listeners.accept().await?;
        moire::task::spawn(handle_conn(link));
    }
}

impl ListenerSet {
    async fn bind(addrs: Vec<ListenAddr>) -> Result<Self> {
        let mut listeners = Vec::with_capacity(addrs.len());

        for addr in addrs {
            listeners.push(BoundListener::bind(addr).await?);
        }

        Ok(Self { listeners })
    }

    async fn accept(&self) -> io::Result<AcceptedLink> {
        loop {
            let pending: Vec<AcceptFuture<'_>> = self
                .listeners
                .iter()
                .enumerate()
                .map(|(index, listener)| listener.accept_boxed(index))
                .collect();

            let ((index, result), _, _) = futures_util::future::select_all(pending).await;
            match result {
                Ok(link) => return Ok(link),
                Err(error) => {
                    tracing::warn!(
                        listener = %self.listeners[index].display_name(),
                        %error,
                        "listener accept failed"
                    );
                }
            }
        }
    }
}

impl BoundListener {
    async fn bind(addr: ListenAddr) -> Result<Self> {
        match addr {
            ListenAddr::UnixSocket(path) => Ok(Self::Unix(UnixSocketListener::bind(path).await?)),
            ListenAddr::Tcp(addr) => Ok(Self::Tcp(TcpSocketListener::bind(addr).await?)),
            ListenAddr::NamedPipe(_) => Err(eyre::eyre!("named pipe listen not yet implemented")),
        }
    }

    fn accept_boxed(&self, index: usize) -> AcceptFuture<'_> {
        match self {
            Self::Unix(listener) => Box::pin(async move { (index, listener.accept().await) }),
            Self::Tcp(listener) => Box::pin(async move { (index, listener.accept().await) }),
        }
    }

    fn display_name(&self) -> String {
        match self {
            Self::Unix(listener) => format!("unix:{}", listener.path),
            Self::Tcp(listener) => format!("tcp:{}", listener.addr),
        }
    }
}

impl UnixSocketListener {
    async fn bind(path: Utf8PathBuf) -> Result<Self> {
        tracing::info!("configured to listen on unix socket {path}");

        let lock = acquire_unix_socket_lock(&path).await?;

        if Path::new(path.as_str()).exists() {
            tracing::info!("removing stale socket at {path}");
            remove_file(path.as_str())?;
        }

        let listener = LocalLinkAcceptor::bind(path.as_str())?;
        tracing::info!("vx daemon listening at unix socket {path}");

        Ok(Self {
            path,
            listener,
            _lock: lock,
        })
    }

    async fn accept(&self) -> io::Result<AcceptedLink> {
        let link = self.listener.accept().await?;
        Ok(AcceptedLink::Local(link))
    }
}

impl TcpSocketListener {
    async fn bind(addr: std::net::SocketAddr) -> Result<Self> {
        tracing::info!("configured to listen on tcp {addr}");

        let listener = TcpListener::bind(addr).await?;
        tracing::info!("vx daemon listening at tcp {addr}");

        Ok(Self { addr, listener })
    }

    async fn accept(&self) -> io::Result<AcceptedLink> {
        let (stream, _) = self.listener.accept().await?;
        Ok(AcceptedLink::Tcp(StreamLink::tcp(stream)))
    }
}

async fn acquire_unix_socket_lock(path: &Utf8Path) -> Result<File> {
    let lock_path = lock_path_for(path);
    let lock_file = File::create(lock_path.as_str())?;

    if lock_file.try_lock_exclusive().is_ok() {
        return Ok(lock_file);
    }

    if unix_socket_is_healthy(path).await {
        return Err(eyre::eyre!("vx daemon already running at {path}"));
    }

    tracing::warn!(
        socket = %path,
        lock = %lock_path,
        "unix socket lock is held, but incumbent daemon is unhealthy; waiting for lock handoff"
    );

    let reacquired = time::timeout(UNIX_LOCK_RECOVERY_TIMEOUT, async {
        loop {
            if lock_file.try_lock_exclusive().is_ok() {
                return Ok::<(), io::Error>(());
            }

            time::sleep(UNIX_LOCK_RETRY_INTERVAL).await;
        }
    })
    .await;

    match reacquired {
        Ok(Ok(())) => Ok(lock_file),
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Err(eyre::eyre!(
            "vx daemon at {path} is unhealthy but still holds lock {lock_path}"
        )),
    }
}

fn lock_path_for(path: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{path}.lock"))
}

async fn unix_socket_is_healthy(path: &Utf8Path) -> bool {
    match time::timeout(DAEMON_HEALTH_TIMEOUT, daemon_status(path)).await {
        Ok(Ok(_status)) => true,
        Ok(Err(error)) => {
            tracing::debug!(socket = %path, %error, "daemon health check failed");
            false
        }
        Err(_) => {
            tracing::debug!(socket = %path, "daemon health check timed out");
            false
        }
    }
}

async fn daemon_status(path: &Utf8Path) -> Result<vx_services::DaemonStatus> {
    let link = LocalLink::connect(path.as_str()).await?;
    let (client, session) = vox::initiator_on(link, TransportMode::Bare)
        .establish::<DaemonClient>(())
        .await?;

    let status = client
        .status()
        .await
        .map_err(|error| eyre::eyre!("daemon status failed: {error:?}"))?;
    drop(session);

    Ok(status)
}

async fn handle_conn(link: AcceptedLink) -> eyre::Result<()> {
    tracing::info!("daemon transport connection");
    let (client, session) = match link {
        AcceptedLink::Local(link) => {
            vox::acceptor_on(link)
                .on_connection(DaemonAcceptor {})
                .establish::<DriverCaller>(DaemonDispatcher::new(DaemonService::new()))
                .await?
        }
        AcceptedLink::Tcp(link) => {
            vox::acceptor_on(link)
                .on_connection(DaemonAcceptor {})
                .establish::<DriverCaller>(DaemonDispatcher::new(DaemonService::new()))
                .await?
        }
    };

    let _session: SessionHandle = session;
    client.closed().await;
    tracing::info!("daemon transport connection closed");
    Ok(())
}

/// A connection acceptor that can serve multiple sub-services:
/// store, runner, vfs, etc.
struct DaemonAcceptor {}

impl ConnectionAcceptor for DaemonAcceptor {
    fn accept(
        &self,
        conn_id: ConnectionId,
        peer_settings: &ConnectionSettings,
        metadata: &[MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        tracing::info!(%conn_id, ?peer_settings, ?metadata, "Sub-connection attempt");

        Err(vec![MetadataEntry {
            key: "error",
            value: MetadataValue::String("oh we have to leak to return metadata, neat"),
            flags: MetadataFlags::NONE,
        }])
    }
}
