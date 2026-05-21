//! Minimal `VxdService` shell during the evaluator purge.

use std::fs;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::watch;
use vx_services::{Daemon, DaemonStatus};

#[derive(Clone)]
pub struct DaemonService {
    inner: Arc<DaemonServiceInner>,
}

struct DaemonServiceInner {
    started_at: Arc<Instant>,
    listen: Vec<String>,
    hosted_services: Vec<String>,
    pid: u32,
    executable_path: Option<String>,
    executable_blake3: Option<String>,
    shutdown_tx: watch::Sender<bool>,
}

impl DaemonService {
    pub fn new(
        listen: Vec<String>,
        hosted_services: Vec<String>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Self {
        let executable_path = std::env::current_exe()
            .ok()
            .and_then(|path| path.into_os_string().into_string().ok());
        let executable_blake3 = executable_path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
        let inner = DaemonServiceInner {
            started_at: Arc::new(Instant::now()),
            listen,
            hosted_services,
            pid: std::process::id(),
            executable_path,
            executable_blake3,
            shutdown_tx,
        };
        Self {
            inner: Arc::new(inner),
        }
    }
}

impl Daemon for DaemonService {
    async fn status(&self) -> DaemonStatus {
        DaemonStatus {
            pid: self.inner.pid,
            uptime_ms: self.inner.started_at.elapsed().as_millis() as u64,
            listen: self.inner.listen.clone(),
            hosted_services: self.inner.hosted_services.clone(),
            executable_path: self.inner.executable_path.clone(),
            executable_blake3: self.inner.executable_blake3.clone(),
        }
    }

    async fn shutdown(&self) {
        let shutdown_tx = self.inner.shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let _ = shutdown_tx.send(true);
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::watch;
    use vx_services::Daemon;

    use super::DaemonService;

    #[tokio::test]
    async fn status_reports_listeners_and_hosted_services() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let service = DaemonService::new(
            vec!["local:///tmp/vixen.vox".to_owned()],
            vec!["Daemon".to_owned(), "Orchestrator".to_owned()],
            shutdown_tx,
        );

        let status = service.status().await;
        assert!(status.pid > 0);
        assert_eq!(status.listen, vec!["local:///tmp/vixen.vox".to_owned()]);
        assert_eq!(
            status.hosted_services,
            vec!["Daemon".to_owned(), "Orchestrator".to_owned()]
        );
        assert!(status.executable_path.is_some());
        assert!(status.executable_blake3.is_some());
    }

    #[tokio::test]
    async fn shutdown_signals_watchers() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let service = DaemonService::new(
            vec!["local:///tmp/vixen.vox".to_owned()],
            vec!["Daemon".to_owned()],
            shutdown_tx,
        );

        service.shutdown().await;
        shutdown_rx
            .changed()
            .await
            .expect("shutdown signal should arrive");
        assert!(*shutdown_rx.borrow());
    }
}
