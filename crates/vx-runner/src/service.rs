use vx_services::{DiscoverProbeRequest, DiscoverResult, ExecRequest, ExecResult, Runner};

use crate::RunnerService;
use crate::exec::run_exec;

impl RunnerService {
    async fn exec(&self, request: ExecRequest) -> ExecResult {
        let Some(vfsd) = self.vfsd() else {
            return ExecResult {
                exit_code: -1,
                stdout: Vec::new(),
                stderr: Vec::new(),
                outputs: Vec::new(),
                error: Some("runner has no CAS-VFS configured for exec".to_string()),
            };
        };
        // Mount → run → flush against the runner's local CAS-VFS.
        run_exec(vfsd, &request).await
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
