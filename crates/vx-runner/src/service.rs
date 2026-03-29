use vx_runner_proto::{
    DiscoverRequest, DiscoverResult, RUNNER_PROTOCOL_VERSION, RunRequest, RunResult, Runner,
};
use vx_store_proto::{ServiceVersion, StoreProgress};

use crate::RunnerService;

impl RunnerService {
    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "vx-runner".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: RUNNER_PROTOCOL_VERSION,
        }
    }

    async fn run(&self, _request: RunRequest, _progress: roam::Tx<StoreProgress>) -> RunResult {
        RunResult {}
    }

    async fn discover(&self, _request: DiscoverRequest) -> DiscoverResult {
        DiscoverResult {}
    }
}

impl Runner for RunnerService {
    async fn version(&self) -> ServiceVersion {
        RunnerService::version(self).await
    }

    async fn run(&self, request: RunRequest, progress: roam::Tx<StoreProgress>) -> RunResult {
        RunnerService::run(self, request, progress).await
    }

    async fn discover(&self, request: DiscoverRequest) -> DiscoverResult {
        RunnerService::discover(self, request).await
    }
}
