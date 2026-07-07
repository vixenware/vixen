pub use exec_protocol::{
    Blake3Hash as ExecBlake3Hash, ByteLen, CapabilityProbeRequest, CapabilityProbeResult,
    ContentHash as ExecContentHash, ExecArg, ExecArgValue, ExecCompletion, ExecControlSignal,
    ExecDemandsSatisfied, ExecDiagnostic, ExecError, ExecErrorKind, ExecExitCode, ExecMount,
    ExecObservationScope, ExecOutcome, ExecPath, ExecPlan, ExecPrefixId, ExecProgram,
    ExecReadObservation, ExecReadSet, ExecRequest, ExecResult, ExecRole, ExecSubfileCompletion,
    ExecText, ExecTree, ExecTreeEvent, ExecTreeFinalization, NodeHash as ExecNodeHash,
    PlatformArch, PlatformName, RUNNER_PROTOCOL_VERSION, Runner, RunnerCapabilities, RunnerClient,
    RunnerDispatcher, RunnerPlatform, RunnerProtocolVersion, RunnerTransport, SandboxCapability,
    ToolchainCapability, ToolchainKind,
};

pub fn exec_role_input_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Executable | ExecRole::Input => vec![ExecPath::from(arg)],
        ExecRole::InputFlag => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_output_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Output => vec![ExecPath::from(arg)],
        ExecRole::OutputFlag => rustc_emit_output_paths(arg),
        _ => Vec::new(),
    }
}

pub fn exec_role_output_dir_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::OutputDir => vec![ExecPath::from(arg)],
        _ => Vec::new(),
    }
}

pub fn exec_role_stdout_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Stdout => vec![ExecPath::from(arg)],
        _ => Vec::new(),
    }
}

pub fn exec_role_env_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Env => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_search_dir_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::SearchDir => vec![ExecPath::from(arg)],
        ExecRole::SearchDirFlag => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_embedded_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    let mut paths = exec_role_input_paths(arg, role);
    paths.extend(exec_role_output_paths(arg, role));
    paths.extend(exec_role_output_dir_paths(arg, role));
    paths.extend(exec_role_stdout_paths(arg, role));
    paths.extend(exec_role_env_paths(arg, role));
    paths.extend(exec_role_search_dir_paths(arg, role));
    paths
}

fn embedded_path_after_equals(arg: &str) -> Option<ExecPath> {
    arg.rsplit_once('=')
        .map(|(_, path)| path)
        .filter(|path| !path.is_empty())
        .map(ExecPath::from)
}

fn rustc_emit_output_paths(arg: &str) -> Vec<ExecPath> {
    let Some(spec) = arg.strip_prefix("--emit=") else {
        return Vec::new();
    };
    spec.split(',')
        .filter_map(|entry| {
            let mut parts = entry.splitn(2, '=');
            let _kind = parts.next()?;
            parts
                .next()
                .filter(|path| !path.is_empty())
                .map(ExecPath::from)
        })
        .collect()
}
