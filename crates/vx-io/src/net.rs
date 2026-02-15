use camino::Utf8Path;
use eyre::Result;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A network endpoint - either TCP or local IPC.
#[derive(Debug, Clone)]
pub enum Endpoint {
    /// TCP endpoint (host + port)
    Tcp { host: String, port: u16 },
    /// Local IPC endpoint (Unix socket path on Unix, named pipe path on Windows)
    Local(camino::Utf8PathBuf),
}

impl Endpoint {
    /// Construct a TCP endpoint.
    pub fn tcp(host: impl Into<String>, port: u16) -> Self {
        Endpoint::Tcp {
            host: host.into(),
            port,
        }
    }

    /// Parse an endpoint string.
    ///
    /// Accepts:
    /// - `host:port` or `tcp://host:port` -> TCP
    /// - `unix:/path/to/socket` -> local IPC endpoint
    /// - Absolute path starting with `/` -> local IPC endpoint (convenience on Unix)
    pub fn parse(endpoint: &str) -> Result<Self> {
        let endpoint = endpoint.trim();

        if let Some(path) = endpoint.strip_prefix("unix:") {
            return Ok(Endpoint::Local(camino::Utf8PathBuf::from(path)));
        }

        #[cfg(unix)]
        {
            if endpoint.starts_with('/') {
                return Ok(Endpoint::Local(camino::Utf8PathBuf::from(endpoint)));
            }
        }

        // TCP: tcp://host:port or host:port
        if let Some(rest) = endpoint.strip_prefix("tcp://") {
            let (host, port) = parse_tcp_host_port(rest)?;
            return Ok(Endpoint::tcp(host, port));
        }

        // Reject unknown schemes
        if endpoint.contains("://") {
            eyre::bail!(
                "unsupported endpoint scheme (expected tcp://, unix:, or host:port): {}",
                endpoint
            );
        }

        let (host, port) = parse_tcp_host_port(endpoint)?;
        Ok(Endpoint::tcp(host, port))
    }

    /// Check if this endpoint is local (loopback or local IPC).
    pub fn is_local(&self) -> bool {
        match self {
            Endpoint::Tcp { host, .. } => is_loopback_tcp_host(host),
            Endpoint::Local(_) => true,
        }
    }

    /// Get a display string for this endpoint.
    pub fn display(&self) -> String {
        match self {
            Endpoint::Tcp { host, port } => format!("{host}:{port}"),
            Endpoint::Local(path) => format!("unix:{}", path),
        }
    }

    /// Get local endpoint path, if this is a local endpoint.
    pub fn local_path(&self) -> Option<&camino::Utf8Path> {
        match self {
            Endpoint::Local(path) => Some(path),
            Endpoint::Tcp { .. } => None,
        }
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

/// A stream that can be either TCP or local IPC stream.
pub enum Stream {
    Tcp(tokio::net::TcpStream),
    Local(roam_local::LocalStream),
    /// On Windows, the server side of a named pipe is a different type
    /// (`NamedPipeServer`) than the client side (`NamedPipeClient`).
    #[cfg(windows)]
    LocalServer(roam_local::LocalServerStream),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Local(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(windows)]
            Stream::LocalServer(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Local(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(windows)]
            Stream::LocalServer(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_flush(cx),
            Stream::Local(s) => Pin::new(s).poll_flush(cx),
            #[cfg(windows)]
            Stream::LocalServer(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Local(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(windows)]
            Stream::LocalServer(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

async fn connect_io(endpoint: &Endpoint) -> io::Result<Stream> {
    match endpoint {
        Endpoint::Tcp { host, port } => {
            let addr = format!("{host}:{port}");
            let stream = tokio::net::TcpStream::connect(addr).await?;
            Ok(Stream::Tcp(stream))
        }
        Endpoint::Local(path) => {
            #[cfg(unix)]
            let local_target = path.as_std_path().to_path_buf();
            #[cfg(windows)]
            let local_target = roam_local::path_to_pipe_name(path.as_std_path());

            let stream = roam_local::connect(local_target).await?;
            Ok(Stream::Local(stream))
        }
    }
}

/// Connect to an endpoint.
pub async fn connect(endpoint: &Endpoint) -> Result<Stream> {
    Ok(connect_io(endpoint).await?)
}

/// Try to connect to an endpoint, returning None if connection refused.
pub async fn try_connect(endpoint: &Endpoint) -> Result<Option<Stream>> {
    match connect_io(endpoint).await {
        Ok(stream) => Ok(Some(stream)),
        Err(io_err) => {
            if io_err.kind() == std::io::ErrorKind::ConnectionRefused {
                return Ok(None);
            }
            if io_err.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            Err(io_err.into())
        }
    }
}

/// A listener that can accept connections from an endpoint.
pub enum Listener {
    Tcp(tokio::net::TcpListener),
    Local {
        listener: roam_local::LocalListener,
        endpoint: camino::Utf8PathBuf,
    },
}

impl Listener {
    /// Bind to an endpoint.
    pub async fn bind(endpoint: &Endpoint) -> Result<Self> {
        match endpoint {
            Endpoint::Tcp { host, port } => {
                let addr = format!("{host}:{port}");
                let listener = tokio::net::TcpListener::bind(addr).await?;
                Ok(Listener::Tcp(listener))
            }
            Endpoint::Local(path) => {
                #[cfg(unix)]
                {
                    if path.exists() {
                        let _ = roam_local::remove_endpoint(path.as_std_path());
                    }
                }

                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                #[cfg(unix)]
                let bind_target = path.as_std_path().to_path_buf();
                #[cfg(windows)]
                let bind_target = roam_local::path_to_pipe_name(path.as_std_path());

                let listener = roam_local::LocalListener::bind(bind_target)?;
                Ok(Listener::Local {
                    listener,
                    endpoint: path.clone(),
                })
            }
        }
    }

    /// Accept a connection, returning the stream and peer address string.
    pub async fn accept(&mut self) -> Result<(Stream, String)> {
        match self {
            Listener::Tcp(listener) => {
                let (stream, addr) =
                    peeps::peep!(listener.accept(), "net.listener.tcp.accept").await?;
                Ok((Stream::Tcp(stream), addr.to_string()))
            }
            Listener::Local { listener, .. } => {
                let stream = peeps::peep!(listener.accept(), "net.listener.local.accept").await?;
                #[cfg(unix)]
                return Ok((Stream::Local(stream), "local".to_string()));
                #[cfg(windows)]
                return Ok((Stream::LocalServer(stream), "local".to_string()));
            }
        }
    }

    /// Get the local address this listener is bound to.
    pub fn local_addr(&self) -> Result<Endpoint> {
        match self {
            Listener::Tcp(listener) => {
                let addr = listener.local_addr()?;
                Ok(Endpoint::tcp(addr.ip().to_string(), addr.port()))
            }
            Listener::Local { endpoint, .. } => Ok(Endpoint::Local(endpoint.clone())),
        }
    }
}

/// Connector used with roam-stream reconnecting clients.
#[derive(Debug, Clone)]
pub struct EndpointConnector {
    endpoint: Endpoint,
}

impl EndpointConnector {
    pub fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }
}

impl roam_stream::Connector for EndpointConnector {
    type Transport = Stream;

    async fn connect(&self) -> io::Result<Self::Transport> {
        connect_io(&self.endpoint).await
    }
}

/// Create a reconnecting roam-stream client for an endpoint.
pub fn reconnecting_client<D>(
    endpoint: Endpoint,
    dispatcher: D,
) -> roam_stream::Client<EndpointConnector, D>
where
    D: roam_stream::ServiceDispatcher + Clone,
{
    reconnecting_client_named(endpoint, dispatcher, None)
}

/// Create a reconnecting roam-stream client for an endpoint with a diagnostic name.
pub fn reconnecting_client_named<D>(
    endpoint: Endpoint,
    dispatcher: D,
    name: Option<String>,
) -> roam_stream::Client<EndpointConnector, D>
where
    D: roam_stream::ServiceDispatcher + Clone,
{
    roam_stream::connect_with_policy(
        EndpointConnector::new(endpoint),
        roam_stream::HandshakeConfig {
            max_payload_size: 256 * 1024 * 1024, // 256 MiB
            name,
            ..Default::default()
        },
        dispatcher,
        roam_stream::RetryPolicy::default(),
    )
}

/// Create a default local IPC endpoint for a service within VX_HOME.
///
/// On Unix this resolves to a Unix domain socket; on Windows the path is
/// mapped to a named pipe by `roam_local`.
pub fn default_local_endpoint(vx_home: &Utf8Path, service: &str) -> Endpoint {
    Endpoint::Local(vx_home.join(format!("{}.sock", service)))
}

// --- Legacy API for backwards compatibility ---

/// Normalize a TCP endpoint string to something `TcpStream::connect` / `TcpListener::bind` accepts.
///
/// Accepts:
/// - `host:port`
/// - `tcp://host:port`
///
/// Rejects other schemes (e.g. `http://`).
pub fn normalize_tcp_endpoint(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim();
    if let Some(rest) = endpoint.strip_prefix("tcp://") {
        return Ok(rest.to_string());
    }
    if endpoint.contains("://") {
        eyre::bail!(
            "unsupported endpoint scheme (expected tcp:// or host:port): {}",
            endpoint
        );
    }
    Ok(endpoint.to_string())
}

/// Best-effort check for whether an endpoint is loopback-local.
///
/// This intentionally does not do DNS resolution. Hostnames other than `localhost`
/// are treated as non-loopback.
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    #[cfg(unix)]
    {
        if endpoint.starts_with("unix:") || endpoint.starts_with('/') {
            return true;
        }
    }

    let Ok(endpoint) = normalize_tcp_endpoint(endpoint) else {
        return false;
    };

    let Ok((host, _port)) = parse_tcp_host_port(&endpoint) else {
        return false;
    };

    is_loopback_tcp_host(&host)
}

/// Check if a TCP host is loopback.
fn is_loopback_tcp_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }

    false
}

fn parse_tcp_host_port(endpoint: &str) -> Result<(String, u16)> {
    if let Ok(addr) = endpoint.parse::<std::net::SocketAddr>() {
        return Ok((addr.ip().to_string(), addr.port()));
    }

    let Some((raw_host, raw_port)) = endpoint.rsplit_once(':') else {
        eyre::bail!("missing TCP port in endpoint: {}", endpoint);
    };

    let host = raw_host
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw_host)
        .trim();

    if host.is_empty() {
        eyre::bail!("missing TCP host in endpoint: {}", endpoint);
    }

    let port: u16 = raw_port
        .parse()
        .map_err(|_| eyre::eyre!("invalid TCP port in endpoint: {}", endpoint))?;

    Ok((host.to_string(), port))
}
