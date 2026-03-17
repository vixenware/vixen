use vx_runner_proto::{
    DiscoverRequest, DiscoverResult, RUNNER_PROTOCOL_VERSION, RunRequest, RunResult, Runner,
};
use vx_store_proto::{ServiceVersion, StoreProgress};

use crate::RunnerService;
use moire::task::FutureExt;

impl RunnerService {
    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "vx-runner".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: RUNNER_PROTOCOL_VERSION,
        }
    }

    async fn run(&self, request: RunRequest, progress: roam::Tx<StoreProgress>) -> RunResult {
        let command = request.program.as_str().to_string();
        tracing::info!(command, "runner.run starting");
        let result = self
            .run_vfs(request, progress)
            .named("runner.run_vfs")
            .await;
        tracing::info!(
            command,
            success = result.success,
            has_observer_value = result.observer_value.is_some(),
            "runner.run finished"
        );
        result
    }

    async fn discover(&self, request: DiscoverRequest) -> DiscoverResult {
        self.discover_vfs(request)
            .named("runner.discover_vfs")
            .await
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
