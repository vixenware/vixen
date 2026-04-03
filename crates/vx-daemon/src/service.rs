//! Minimal `VxdService` shell during the evaluator purge.

use std::sync::Arc;
use std::time::Instant;

use vx_services::{Daemon, DaemonStatus};

#[derive(Clone)]
pub struct DaemonService {
    inner: Arc<DaemonServiceInner>,
}

struct DaemonServiceInner {
    started_at: Arc<Instant>,
    listen: Vec<String>,
    hosted_services: Vec<String>,
}

impl DaemonService {
    pub fn new(listen: Vec<String>, hosted_services: Vec<String>) -> Self {
        let inner = DaemonServiceInner {
            started_at: Arc::new(Instant::now()),
            listen,
            hosted_services,
        };
        Self {
            inner: Arc::new(inner),
        }
    }
}

impl Daemon for DaemonService {
    async fn status(&self) -> DaemonStatus {
        DaemonStatus {
            uptime_ms: self.inner.started_at.elapsed().as_millis() as u64,
            listen: self.inner.listen.clone(),
            hosted_services: self.inner.hosted_services.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use vx_services::Daemon;

    use super::DaemonService;

    #[tokio::test]
    async fn status_reports_listeners_and_hosted_services() {
        let service = DaemonService::new(
            vec!["local:///tmp/vixen.vox".to_owned()],
            vec!["Daemon".to_owned(), "Orchestrator".to_owned()],
        );

        let status = service.status().await;
        assert_eq!(status.listen, vec!["local:///tmp/vixen.vox".to_owned()]);
        assert_eq!(
            status.hosted_services,
            vec!["Daemon".to_owned(), "Orchestrator".to_owned()]
        );
    }
}
