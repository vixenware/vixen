use std::{
    fs::{self, File},
    time::{SystemTime, UNIX_EPOCH},
};

use camino::Utf8PathBuf;
use fs4::fs_std::FileExt;

use crate::{AcceptedLink, UnixSocketListener, handle_conn, lock_path_for, unix_socket_is_healthy};

fn unique_socket_path(name: &str) -> Utf8PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let pid = std::process::id();
    Utf8PathBuf::from(format!("/tmp/vx-daemon-{name}-{pid}-{nonce}.sock"))
}

#[tokio::test]
async fn health_check_reports_live_unix_daemon() {
    let path = unique_socket_path("healthy");
    let lock_path = lock_path_for(path.as_path());
    let lock_file = File::create(lock_path.as_str()).unwrap();
    lock_file.try_lock_exclusive().unwrap();

    let listener = vox_stream::LocalLinkAcceptor::bind(path.as_str()).unwrap();
    let accept_task = tokio::spawn(async move {
        let link = listener.accept().await.unwrap();
        handle_conn(AcceptedLink::Local(link)).await.unwrap();
    });

    assert!(unix_socket_is_healthy(path.as_path()).await);

    accept_task.abort();
    let _ = fs::remove_file(path.as_str());
    let _ = fs::remove_file(lock_path.as_str());
}

#[tokio::test]
async fn stale_socket_is_reclaimed_when_lock_is_free() {
    let path = unique_socket_path("stale");
    fs::write(path.as_str(), b"stale").unwrap();

    let listener = UnixSocketListener::bind(path.clone()).await.unwrap();
    assert!(path.exists());

    drop(listener);

    let lock_path = lock_path_for(path.as_path());
    let _ = fs::remove_file(path.as_str());
    let _ = fs::remove_file(lock_path.as_str());
}
