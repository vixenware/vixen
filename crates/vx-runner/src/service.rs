use vx_runtime::observe::{
    CompletedRun, Observe, ObserveRequest, Observed, WireValue, evaluate_observer,
};
use vx_runtime::run_value::ProcOutcome;
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

    async fn observe(&self, request: ObserveRequest) -> Observed {
        let Some(vfsd) = self.vfsd() else {
            return Observed::Poisoned(
                "runner-backed observe has no CAS-VFS configured for exec".to_string(),
            );
        };
        let result = run_exec(
            vfsd,
            &ExecRequest {
                program: request.command.program,
                args: request.command.args,
                env: Vec::new(),
                mounts: Vec::new(),
                outputs: Vec::new(),
            },
        )
        .await;
        if let Some(error) = result.error {
            return Observed::Poisoned(error);
        }
        let observed = evaluate_observer(
            CompletedRun::new(ProcOutcome {
                exit_code: result.exit_code,
                stdout: result.stdout,
                stderr: result.stderr,
            }),
            request.observer,
        );
        match observed {
            Observed::Value(WireValue::Tree) => Observed::Poisoned(
                "runner-backed observe returned a tree, but Producing output serving from \
                 flushed CAS trees is not wired yet"
                    .to_string(),
            ),
            other => other,
        }
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

impl Observe for RunnerService {
    async fn observe(&self, request: ObserveRequest) -> Observed {
        RunnerService::observe(self, request).await
    }
}
