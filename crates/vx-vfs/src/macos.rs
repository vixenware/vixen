//! macOS VFS startup and mount orchestration.
//!
//! This module relies on the Vixen.app FSKit host in this repository (`apps/mac`).
//!
//! `Vixen` implements a `roam` bridge between:
//! - an FSKit extension running in a sandboxed process, and
//! - `vx-vfs`, which serves VFS connections over SHM.

use crate::types::{VfsConfig, VfsError, VfsStart};
use moire::process::Command;
use moire::sync::mpsc;
use moire::sync::watch;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{BareConduit, Driver};
use roam_shm::{AddPeerOptions, Segment, SegmentConfig, ShmHost, SizeClassConfig};
use roam_types::Parity;
use shm_primitives::FileCleanup;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vixenfs_proto::{VfsDispatcher, VfsServer as Vfs};
use vx_vfs_types::{VIXEN_MOUNT_HELPER_PATH, VfsAction, VfsDiagnostic, VfsIssueCode};
const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_GC_AGE: Duration = Duration::from_secs(6 * 60 * 60);
const SHM_BOOTSTRAP_REQ_MAGIC: &[u8; 4] = b"RSH0";
const SHM_BOOTSTRAP_RESP_MAGIC: &[u8; 4] = b"RSP0";
const SHM_BOOTSTRAP_STATUS_OK: u8 = 0;
const SHM_BOOTSTRAP_STATUS_ERR: u8 = 1;

#[derive(Clone, Debug)]
struct SessionPaths {
    sid: String,
    session_dir: PathBuf,
    shm_path: PathBuf,
}

impl SessionPaths {
    fn new(root: &Path, sid: impl Into<String>) -> Result<Self, String> {
        let sid = sid.into();
        if !is_valid_sid(&sid) {
            return Err("sid must be 32 hex chars or UUID format".to_string());
        }
        let sessions_root = root.join("sessions");
        let session_dir = sessions_root.join(&sid);
        let shm_path = session_dir.join("hub.shm");
        Ok(Self {
            sid,
            session_dir,
            shm_path,
        })
    }

    fn sid(&self) -> &str {
        &self.sid
    }

    fn session_dir(&self) -> PathBuf {
        self.session_dir.clone()
    }

    fn shm_path(&self) -> PathBuf {
        self.shm_path.clone()
    }

    fn validate_containment(&self) -> Result<(), String> {
        if self
            .session_dir
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("session path escapes root".to_string());
        }
        Ok(())
    }

    fn ensure_dirs(&self) -> std::io::Result<()> {
        if let Some(parent) = self.session_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&self.session_dir)?;
        Ok(())
    }
}

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tracing::debug!(mount_path = %config.mount_path, "entering macos vfs startup");

    let (mount_ready_tx, mut mount_ready_rx) = moire::sync::mpsc::channel("mount_ready", 2);
    let (vfs_dead_tx, vfs_dead_rx) = moire::sync::watch::channel("vfs_dead", false);
    let sid = spawn_shm_loop(vfs, mount_ready_tx, vfs_dead_tx)?;

    let debug_mode = std::env::var("VIXENFS_DEBUG").as_deref() == Ok("1");

    tracing::debug!(%sid, mount_path = %config.mount_path, debug_mode, "calling vx_fskit::setup");

    // vx_fskit::setup handles: prerequisites, stale mount cleanup, mkdir,
    // /sbin/mount -F -t vixenfs, and health.json verification.
    let fskit_result = vx_fskit::setup(&sid, &config.mount_path, debug_mode).await;

    if let Some(ref error) = fskit_result.error {
        cleanup_session_dir(&sid).await;
        let log_detail = fskit_result
            .log
            .iter()
            .map(|entry| {
                let prefix = match entry.level {
                    vx_fskit::LogLevel::Ok => "\u{2713}", // ✓
                    vx_fskit::LogLevel::Info => "",
                    vx_fskit::LogLevel::Fail => "\u{2717}", // ✗
                };
                format!("{prefix} {}", entry.message)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(VfsError::MountFailed {
            message: error.summary.clone(),
            diagnostics: vec![VfsDiagnostic {
                code: VfsIssueCode::MountFailed,
                summary: error.summary.clone(),
                detail: Some(log_detail),
                actions: error
                    .remediation
                    .iter()
                    .map(|r| VfsAction::RunCommand(r.clone()))
                    .collect(),
            }],
        });
    }

    // Mount syscall succeeded and health.json confirms the extension is up.
    // Now wait for the SHM connection (extension connecting to our SHM segment).
    tracing::debug!("fskit mount ok, waiting for SHM readiness");

    match tokio::time::timeout(MOUNT_TIMEOUT, mount_ready_rx.recv()).await {
        Ok(Some(())) => {
            tracing::info!(mount_path = %config.mount_path, "VFS mounted and SHM connected");
            Ok(VfsStart::Mounted {
                mount_path: config.mount_path,
                vfs_dead: vfs_dead_rx,
            })
        }
        Ok(None) => {
            cleanup_session_dir(&sid).await;
            Err(VfsError::CommandFailed {
                program: "vx-vfs",
                message: "SHM readiness channel closed before extension connected".to_string(),
                _diagnostics: vec![],
            })
        }
        Err(_) => {
            cleanup_session_dir(&sid).await;
            Err(VfsError::CommandFailed {
                program: "vx-vfs",
                message: format!(
                    "FSKit extension did not connect via SHM within {MOUNT_TIMEOUT:?}"
                ),
                _diagnostics: vec![],
            })
        }
    }
}

fn spawn_shm_loop<T>(
    vfs: T,
    mount_ready_tx: mpsc::Sender<()>,
    _vfs_dead_tx: watch::Sender<bool>,
) -> Result<String, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    let (sid_tx, sid_rx) = std::sync::mpsc::channel::<Result<String, String>>();

    std::thread::Builder::new()
        .name("vfs-runtime".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_name("vfs-worker")
                .enable_all()
                .build()
                .expect("failed to create VFS tokio runtime");
            rt.block_on(async move {
                let app_group_root = match resolve_app_group_path().await {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!("failed to resolve App Group path: {error}")));
                        return;
                    }
                };

                let sid = random_sid_hex();
                let session_paths = match SessionPaths::new(&app_group_root, sid.clone()) {
                    Ok(paths) => paths,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!("failed to build session paths: {error}")));
                        return;
                    }
                };

                if let Err(error) =
                    gc_stale_sessions(&app_group_root, session_paths.sid(), SESSION_GC_AGE).await
                {
                    tracing::warn!(error = %error, "failed to garbage-collect stale SHM sessions");
                }

                if let Err(error) = session_paths.validate_containment() {
                    let _ = sid_tx.send(Err(format!("invalid SHM session path containment: {error}")));
                    return;
                }

                if let Err(error) = session_paths.ensure_dirs() {
                    let _ = sid_tx.send(Err(format!("failed to create session directory: {error}")));
                    return;
                }

                let shm_path = session_paths.shm_path();
                if moire::fs::try_exists(&shm_path).await.unwrap_or(false)
                    && let Err(error) = moire::fs::remove_file(&shm_path).await
                {
                    let _ = sid_tx.send(Err(format!(
                        "failed to remove stale shm file {}: {error}",
                        shm_path.display()
                    )));
                    return;
                }

                let classes = [SizeClassConfig {
                    slot_size: 4096,
                    slot_count: 64,
                }];
                let segment = match Segment::create(
                    &shm_path,
                    SegmentConfig {
                        max_guests: 16,
                        bipbuf_capacity: 64 * 1024,
                        max_payload_size: 1024 * 1024,
                        inline_threshold: 256,
                        heartbeat_interval: 0,
                        size_classes: &classes,
                    },
                    FileCleanup::Manual,
                ) {
                    Ok(segment) => std::sync::Arc::new(segment),
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!(
                            "failed to create SHM hub {}: {error}",
                            shm_path.display()
                        )));
                        return;
                    }
                };

                let host = ShmHost::new(segment.clone());

                let control_sock_path = control_socket_path_for_sid(&app_group_root, session_paths.sid());
                if moire::fs::try_exists(&control_sock_path)
                    .await
                    .unwrap_or(false)
                    && let Err(error) = moire::fs::remove_file(&control_sock_path).await
                {
                    let _ = sid_tx.send(Err(format!(
                        "failed to remove stale SHM control socket {}: {error}",
                        control_sock_path.display()
                    )));
                    return;
                }

                let control_listener = match tokio::net::UnixListener::bind(&control_sock_path) {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!(
                            "failed to bind SHM control socket {}: {error}",
                            control_sock_path.display()
                        )));
                        return;
                    }
                };

                let _ = sid_tx.send(Ok(sid.clone()));

                loop {
                    let (mut control_stream, _peer) = match control_listener.accept().await {
                        Ok(parts) => parts,
                        Err(error) => {
                            tracing::warn!(%error, "failed to accept SHM control connection");
                            continue;
                        }
                    };

                    let request_sid = match recv_bootstrap_request(&mut control_stream).await {
                        Ok(sid) => sid,
                        Err(error) => {
                            tracing::warn!(%error, "failed to read SHM bootstrap request");
                            continue;
                        }
                    };
                    if request_sid != session_paths.sid() {
                        let _ = send_bootstrap_error(&mut control_stream, "sid mismatch").await;
                        tracing::warn!(
                            expected = %session_paths.sid(),
                            got = %request_sid,
                            "SHM bootstrap sid mismatch"
                        );
                        continue;
                    }

                    let prepared = match host.add_peer(AddPeerOptions::default()) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            let _ = send_bootstrap_error(
                                &mut control_stream,
                                format!("failed to reserve peer: {error}"),
                            )
                            .await;
                            tracing::warn!(%error, "failed to reserve SHM peer ticket");
                            continue;
                        }
                    };
                    let (host_peer, guest_ticket) = prepared.into_parts();
                    let peer_id = host_peer.peer_id();
                    let hub_path = shm_path.to_string_lossy().to_string();
                    let doorbell_fd = guest_ticket.doorbell.as_raw_fd();
                    let mmap_control_fd = guest_ticket.mmap_tx_fd;

                    let shm_file = match std::fs::File::open(&shm_path) {
                        Ok(file) => file,
                        Err(error) => {
                            let _ = send_bootstrap_error(
                                &mut control_stream,
                                format!("failed to open shm file: {error}"),
                            )
                            .await;
                            tracing::warn!(%error, "failed to open SHM file");
                            host_peer.release_reservation();
                            continue;
                        }
                    };
                    let shm_fd = shm_file.as_raw_fd();

                    let send_result = send_bootstrap_ok(
                        &mut control_stream,
                        peer_id.get(),
                        &hub_path,
                        &[doorbell_fd, shm_fd, mmap_control_fd],
                    )
                    .await;
                    drop(shm_file);
                    // mmap tx fd is an owned raw fd on our side; close after passing.
                    unsafe {
                        libc::close(mmap_control_fd);
                    }

                    if let Err(error) = send_result {
                        tracing::warn!(%error, peer = %peer_id.get(), "failed SHM bootstrap ticket exchange");
                        host_peer.release_reservation();
                        continue;
                    }

                    tracing::debug!(
                        sid = %session_paths.sid(),
                        peer = %peer_id.get(),
                        "issued SHM bootstrap ticket"
                    );
                    let _ = mount_ready_tx.try_send(());

                    let vfs_dispatcher = VfsDispatcher::new(vfs.clone());
                    moire::task::spawn(
                        async move {
                            let link = match host_peer.into_link() {
                                Ok(link) => link,
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        peer = %peer_id.get(),
                                        "failed to build host link for peer"
                                    );
                                    return;
                                }
                            };
                            let conduit: BareConduit<roam_types::MessageFamily, roam_shm::ShmLink> =
                                BareConduit::new(link);
                            let (mut session, handle, _session_handle) = match acceptor(conduit)
                                .establish()
                                .await
                            {
                                Ok(parts) => parts,
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        peer = %peer_id.get(),
                                        "failed SHM session establish"
                                    );
                                    return;
                                }
                            };
                            moire::task::spawn(
                                async move { session.run().await }.named("shm_host_session"),
                            );
                            let mut driver = Driver::new(handle, vfs_dispatcher, Parity::Even);
                            driver.run().await;
                            tracing::debug!(peer = %peer_id.get(), "SHM peer driver exited");
                        }
                        .named("shm_host_driver"),
                    );
                }
            });
        })
        .expect("failed to spawn VFS runtime thread");

    sid_rx
        .recv()
        .unwrap_or(Err("VFS runtime thread died before sending sid".to_string()))
        .map_err(|msg| VfsError::CommandFailed {
            program: "mount",
            message: msg,
            _diagnostics: vec![],
        })
}

fn random_sid_hex() -> String {
    let mut sid = String::with_capacity(32);
    for _ in 0..32 {
        let nibble = fastrand::u8(0..16);
        let ch = match nibble {
            0..=9 => (b'0' + nibble) as char,
            _ => (b'a' + (nibble - 10)) as char,
        };
        sid.push(ch);
    }
    sid
}

fn is_valid_sid(sid: &str) -> bool {
    if sid.len() == 32 {
        return sid.chars().all(|c| c.is_ascii_hexdigit());
    }
    if sid.len() == 36 {
        for (i, c) in sid.chars().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                if c != '-' {
                    return false;
                }
            } else if !c.is_ascii_hexdigit() {
                return false;
            }
        }
        return true;
    }
    false
}

async fn recv_bootstrap_request(stream: &mut tokio::net::UnixStream) -> Result<String, String> {
    use tokio::io::AsyncReadExt;

    let mut magic = [0u8; 4];
    stream
        .read_exact(&mut magic)
        .await
        .map_err(|e| format!("read bootstrap magic: {e}"))?;
    if &magic != SHM_BOOTSTRAP_REQ_MAGIC {
        return Err("invalid bootstrap request magic".to_string());
    }

    let mut len_bytes = [0u8; 2];
    stream
        .read_exact(&mut len_bytes)
        .await
        .map_err(|e| format!("read sid length: {e}"))?;
    let sid_len = u16::from_le_bytes(len_bytes) as usize;
    let mut sid_bytes = vec![0u8; sid_len];
    stream
        .read_exact(&mut sid_bytes)
        .await
        .map_err(|e| format!("read sid payload: {e}"))?;
    let sid = String::from_utf8(sid_bytes).map_err(|e| format!("sid not utf-8: {e}"))?;
    if !is_valid_sid(&sid) {
        return Err("invalid sid encoding".to_string());
    }
    Ok(sid)
}

async fn send_bootstrap_error(
    stream: &mut tokio::net::UnixStream,
    message: impl AsRef<str>,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let payload = message.as_ref().as_bytes();
    let len = u16::try_from(payload.len()).unwrap_or(u16::MAX);
    let payload = &payload[..usize::from(len)];

    let mut response = Vec::with_capacity(8 + payload.len());
    response.extend_from_slice(SHM_BOOTSTRAP_RESP_MAGIC);
    response.push(SHM_BOOTSTRAP_STATUS_ERR);
    response.push(0);
    response.extend_from_slice(&len.to_le_bytes());
    response.extend_from_slice(payload);
    stream.write_all(&response).await
}

async fn send_bootstrap_ok(
    stream: &mut tokio::net::UnixStream,
    peer_id: u8,
    hub_path: &str,
    fds: &[i32],
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let payload = hub_path.as_bytes();
    let len = u16::try_from(payload.len()).unwrap_or(u16::MAX);
    let payload = &payload[..usize::from(len)];

    let mut response = Vec::with_capacity(8 + payload.len());
    response.extend_from_slice(SHM_BOOTSTRAP_RESP_MAGIC);
    response.push(SHM_BOOTSTRAP_STATUS_OK);
    response.push(peer_id);
    response.extend_from_slice(&len.to_le_bytes());
    response.extend_from_slice(payload);
    stream.write_all(&response).await?;
    send_fds(stream.as_raw_fd(), fds)
}

fn send_fds(sock_fd: i32, fds: &[i32]) -> std::io::Result<()> {
    if fds.is_empty() {
        return Ok(());
    }

    let mut data = [0xA5u8];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };

    // SAFETY: size is computed from fds slice length.
    let control_len =
        unsafe { libc::CMSG_SPACE((std::mem::size_of::<i32>() * fds.len()) as u32) } as usize;
    let mut control = vec![0u8; control_len];

    // SAFETY: zeroed msghdr is valid before initialization.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len() as _;

    // SAFETY: cmsg buffer is allocated with CMSG_SPACE above.
    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if cmsg.is_null() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "failed to allocate control message header",
        ));
    }

    // SAFETY: cmsg points into msg control buffer and has enough space for fds.
    unsafe {
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN((std::mem::size_of::<i32>() * fds.len()) as u32) as _;
        let data_ptr = libc::CMSG_DATA(cmsg).cast::<i32>();
        std::ptr::copy_nonoverlapping(fds.as_ptr(), data_ptr, fds.len());
    }

    // SAFETY: msghdr points to valid buffers.
    let sent = unsafe { libc::sendmsg(sock_fd, &msg, 0) };
    if sent < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if sent == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "sendmsg wrote zero bytes",
        ));
    }
    Ok(())
}

async fn gc_stale_sessions(root: &Path, keep_sid: &str, max_age: Duration) -> Result<(), String> {
    let sessions_root = root.join("sessions");
    if !moire::fs::try_exists(&sessions_root).await.unwrap_or(false) {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&sessions_root)
        .await
        .map_err(|error| format!("failed to list {}: {error}", sessions_root.display()))?;
    let now = std::time::SystemTime::now();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("failed to read sessions entry: {error}"))?
    {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == keep_sid {
            continue;
        }
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < max_age {
            continue;
        }
        if let Err(error) = moire::fs::remove_dir_all(&path).await {
            tracing::warn!(path = %path.display(), %error, "failed to remove stale SHM session");
        }
    }
    Ok(())
}

fn control_socket_path_for_sid(root: &Path, sid: &str) -> PathBuf {
    root.join(format!("c-{sid}.sock"))
}

async fn cleanup_session_dir(sid: &str) {
    let app_group_root = match resolve_app_group_path().await {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%sid, %error, "failed to resolve App Group path for SHM cleanup");
            return;
        }
    };
    let control_sock_path = control_socket_path_for_sid(&app_group_root, sid);
    match moire::fs::remove_file(&control_sock_path).await {
        Ok(()) => tracing::debug!(
            %sid,
            path = %control_sock_path.display(),
            "removed SHM control socket"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            %sid,
            path = %control_sock_path.display(),
            %error,
            "failed to remove SHM control socket"
        ),
    }

    if !is_valid_sid(sid) {
        tracing::warn!(%sid, "invalid sid during SHM cleanup");
        return;
    }
    let session_paths = match SessionPaths::new(&app_group_root, sid.to_string()) {
        Ok(paths) => paths,
        Err(error) => {
            tracing::warn!(%sid, %error, "failed to construct SHM cleanup paths");
            return;
        }
    };
    if let Err(error) = session_paths.validate_containment() {
        tracing::warn!(%sid, %error, "refusing SHM cleanup outside App Group root");
        return;
    }
    let session_dir = session_paths.session_dir();
    match moire::fs::remove_dir_all(&session_dir).await {
        Ok(()) => {
            tracing::debug!(%sid, path = %session_dir.display(), "removed SHM session directory");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                %sid,
                path = %session_dir.display(),
                %error,
                "failed to remove SHM session directory"
            );
        }
    }
}

/// Resolve the App Group container path via the `vixenfs-mount` helper.
///
/// The helper is codesigned with the App Group entitlement, so it can call
/// `FileManager.containerURL(forSecurityApplicationGroupIdentifier:)` and
/// return the resolved path. This lets VXRunner (a plain Rust binary) discover
/// the shared container without needing its own App Group entitlement.
pub async fn resolve_app_group_path() -> Result<PathBuf, String> {
    let output = Command::new(VIXEN_MOUNT_HELPER_PATH)
        .arg("--app-group-path")
        .output()
        .await
        .map_err(|e| format!("failed to run {VIXEN_MOUNT_HELPER_PATH}: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "vixenfs-mount --app-group-path failed (status={:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let path = String::from_utf8(output.stdout)
        .map_err(|e| format!("vixenfs-mount --app-group-path returned invalid UTF-8: {e}"))?;
    let path = path.trim();
    if path.is_empty() {
        return Err("vixenfs-mount --app-group-path returned empty path".to_string());
    }

    Ok(PathBuf::from(path))
}
