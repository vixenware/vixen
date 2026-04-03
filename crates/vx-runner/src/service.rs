use vx_services::{DiscoverProbeRequest, DiscoverResult, ExecRequest, ExecResult, Runner};

use crate::RunnerService;

impl RunnerService {
    async fn exec(&self, _request: ExecRequest) -> ExecResult {
        ExecResult {}
    }

    async fn discover_probe(&self, _request: DiscoverProbeRequest) -> DiscoverResult {
        DiscoverResult {}
    }
}

impl Runner for RunnerService {
    async fn exec(&self, request: ExecRequest) -> ExecResult {
        RunnerService::exec(self, request).await
    }

    async fn discover_probe(&self, request: DiscoverProbeRequest) -> DiscoverResult {
        RunnerService::discover_probe(self, request).await
    }
}
