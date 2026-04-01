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
}

impl DaemonService {
    pub fn new() -> Self {
        let inner = DaemonServiceInner {
            started_at: Arc::new(Instant::now()),
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
        }
    }
}
