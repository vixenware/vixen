use std::fs;
use std::path::{Path, PathBuf};

const GUARDED_CRATES: &[&str] = &["vx-runtime-core", "vx-runtime-driver"];

const FORBIDDEN_NEEDLES: &[&str] = &[
    "vx_ir",
    "vx-ir",
    "ExprKind",
    "ExprId",
    "StmtKind",
    "StmtId",
    "PatternKind",
    "PatternId",
    "VirProgram",
    "BodyId",
    "ItemKind",
    "node_exprs",
    "exprmap",
    "eval_inner",
    "eval_expr",
    "walk_expr",
    "legacy_eval",
    "fn force",
    ".force(",
    "pub fn root(",
    "pub fn boundary_payload",
    "pub fn public_value_field",
    "pub fn test_probe",
];

const RUNTIME_TREE_WALKER_NEEDLES: &[&str] = &[
    "struct Evaluator",
    "impl Evaluator",
    "fn eval_inner",
    "fn force_node",
    "fn force_frame_binding",
    "enum NodeMemo",
    "struct NodeRef",
    "ThunkState::Unforced",
    "ThunkState::Node",
    "Value::Closure",
    "pub mod dispatch",
    "crate::dispatch",
    "DispatchPlan",
    "struct Dispatcher",
    "dispatch_scalar",
];

const RUNTIME_EXEC_OWNERSHIP_NEEDLES: &[&str] = &[
    "pub struct ExecCommand",
    "pub struct ObserveRequest",
    "pub enum WireValue",
    "pub enum Observed",
    "pub trait Observe",
    "pub struct ObserveService",
    "struct ObserveService",
    "ScratchObserveService",
    "local_observer",
    "fn scratch_dir",
    "fn capture_outputs",
    "tokio::process::Command",
];

const RUNTIME_CACHE_OWNERSHIP_NEEDLES: &[&str] = &[
    "struct BoundaryL2Cache",
    "impl BoundaryL2Cache",
    "VX_RUNTIME_L2_DIR",
    "L2_WRITE_COUNTER",
    "eval_body_with_cache_dir",
    "eval_body_with_exec_and_cache_dir",
    "run_body_with_cache_dir",
    "run_body_with_exec_and_cache_dir",
    "with_local_index_dir",
    "LocalIndexCacheLayer",
    "vx_cache_index",
    "vx-cache-index",
    "fn cache_path",
    "fn read_json",
    "fn write_json",
];

const GRAPH_EXEC_CACHE_INDEX_NEEDLES: &[&str] = &[
    "LocalCacheIndex",
    "vx_cache_index",
    "host_exec_cache",
    "exec_invocation_cache",
    "host-exec-v1",
    "exec-value-v1",
    "function-output-v1",
];

const RUNTIME_CACHE_DIRECT_LOCAL_INDEX_NEEDLES: &[&str] = &[
    "local_index: Option<LocalCacheIndex>",
    "local_index: Some(LocalCacheIndex::new",
];

const RUNTIME_CACHE_SERVICE_CLIENT_NEEDLES: &[&str] = &[
    "vx_services",
    "vx-services",
    "StoreClient",
    "CacheIndexClient",
    "CacheLookupRequest",
    "CachePutRequest",
    "CacheRecordNode",
    "CacheRecordBody",
    "CacheInputKeyDigest",
    "CasNode::CacheRecord",
    ".put_node(",
    ".get_node(",
];

const KERNEL_MODE_ORACLE_NEEDLES: &[&str] = &[
    "KernelMode::Off",
    "KernelMode::Interp",
    "KernelMode::Jit",
    "\n    Off,",
    "\n    Interp,",
    "\n    Jit,",
];

const PUBLIC_NATIVE_THUNK_NEEDLES: &[&str] = &["pub fn native("];

const PUBLIC_OUTPUT_TREE_MARKER_NEEDLES: &[&str] = &[
    "impl ExecutorOutputTree {\n    pub fn new",
    "impl Default for ExecutorOutputTree",
];

#[test]
fn execution_crates_stay_source_blind() {
    let root = workspace_root();
    let files = GUARDED_CRATES
        .iter()
        .flat_map(|krate| {
            let crate_dir = root.join("crates").join(krate);
            guarded_files(&crate_dir)
        })
        .collect::<Vec<_>>();

    assert_no_forbidden_needles(
        &root,
        files,
        FORBIDDEN_NEEDLES,
        "execution crates must remain source-blind",
        "forbidden runtime source affordance",
    );
}

#[test]
fn runtime_tree_walker_affordances_stay_deleted() {
    let root = workspace_root();
    let dispatch_rs = root.join("crates/vx-runtime/src/dispatch.rs");
    assert!(
        !dispatch_rs.exists(),
        "{} must stay deleted; graph execution must not regain the scalar dispatch planner",
        dispatch_rs
            .strip_prefix(&root)
            .unwrap_or(&dispatch_rs)
            .display()
    );

    let mut files = Vec::new();
    collect_rs_files(&root.join("crates/vx-runtime/src"), &mut files);

    assert_no_forbidden_needles(
        &root,
        files,
        RUNTIME_TREE_WALKER_NEEDLES,
        "runtime tree-walker affordances must stay deleted",
        "deleted tree-walker affordance",
    );
}

#[test]
fn runtime_exec_spawning_stays_quarantined() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates/vx-runtime/src"), &mut files);

    assert_no_forbidden_needles(
        &root,
        files,
        RUNTIME_EXEC_OWNERSHIP_NEEDLES,
        "runtime exec process spawning must stay deleted",
        "runtime-local observe/process affordance",
    );
}

#[test]
fn runtime_cache_file_index_stays_out_of_graph_exec() {
    let root = workspace_root();
    let mut files = vec![root.join("crates/vx-runtime/Cargo.toml")];
    collect_rs_files(&root.join("crates/vx-runtime/src"), &mut files);

    assert_no_forbidden_needles(
        &root,
        files,
        RUNTIME_CACHE_OWNERSHIP_NEEDLES,
        "runtime graph execution must not own the L2 file index",
        "runtime-owned cache-index affordance",
    );
}

#[test]
fn graph_exec_does_not_own_cache_index_details() {
    let root = workspace_root();
    let graph_exec = root.join("crates/vx-runtime/src/graph_exec.rs");

    assert_no_forbidden_needles(
        &root,
        vec![graph_exec],
        GRAPH_EXEC_CACHE_INDEX_NEEDLES,
        "graph_exec must not own L1 maps or temporary cache-index layout",
        "graph_exec-owned cache detail",
    );
}

#[test]
fn runtime_cache_uses_a_persistent_layer_adapter() {
    let root = workspace_root();
    let cache_rs = root.join("crates/vx-runtime/src/cache.rs");

    assert_no_forbidden_needles(
        &root,
        vec![cache_rs],
        RUNTIME_CACHE_DIRECT_LOCAL_INDEX_NEEDLES,
        "RuntimeCache must not directly own the temporary LocalCacheIndex",
        "direct local-index field",
    );
}

#[test]
fn runtime_cache_does_not_import_service_clients() {
    let root = workspace_root();
    let mut files = vec![root.join("crates/vx-runtime/Cargo.toml")];
    collect_rs_files(&root.join("crates/vx-runtime/src"), &mut files);

    assert_no_forbidden_needles(
        &root,
        files,
        RUNTIME_CACHE_SERVICE_CLIENT_NEEDLES,
        "vx-runtime must not own daemon CacheIndex/Store RPC clients",
        "runtime-owned cache service affordance",
    );
}

#[test]
fn kernel_mode_oracle_labels_stay_deleted() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates/vx-runtime/src"), &mut files);
    collect_rs_files(&root.join("crates/vx-runtime/tests"), &mut files);
    collect_rs_files(&root.join("crates/vx-runner/src"), &mut files);
    collect_rs_files(&root.join("crates/vx-daemon/src"), &mut files);
    files.retain(|path| {
        path.file_name().and_then(|name| name.to_str()) != Some("source_blind_guardrails.rs")
    });

    assert_no_forbidden_needles(
        &root,
        files,
        KERNEL_MODE_ORACLE_NEEDLES,
        "KernelMode must not regain old tree-walker-oracle labels",
        "old KernelMode oracle label",
    );
}

#[test]
fn native_thunk_constructor_stays_crate_private() {
    let root = workspace_root();
    let eval_rs = root.join("crates/vx-runtime/src/eval.rs");

    assert_no_forbidden_needles(
        &root,
        vec![eval_rs],
        PUBLIC_NATIVE_THUNK_NEEDLES,
        "external callers must not manufacture native graph-boundary thunks",
        "public native thunk constructor",
    );
}

#[test]
fn executor_output_tree_marker_stays_sealed() {
    let root = workspace_root();
    let eval_rs = root.join("crates/vx-runtime/src/eval.rs");

    assert_no_forbidden_needles(
        &root,
        vec![eval_rs],
        PUBLIC_OUTPUT_TREE_MARKER_NEEDLES,
        "external callers must not manufacture executor-local output tree markers",
        "public executor output tree marker constructor",
    );
}

fn assert_no_forbidden_needles(
    root: &Path,
    files: Vec<PathBuf>,
    needles: &[&str],
    assertion: &str,
    label: &str,
) {
    let mut failures = Vec::new();

    for path in files {
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

        for needle in needles {
            if text.contains(needle) {
                failures.push(format!(
                    "{} contains {label} `{needle}`",
                    path.strip_prefix(root).unwrap_or(&path).display()
                ));
            }
        }
    }

    assert!(failures.is_empty(), "{assertion}:\n{}", failures.join("\n"));
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("runtime crate lives under crates/")
        .to_path_buf()
}

fn guarded_files(crate_dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![crate_dir.join("Cargo.toml")];
    collect_rs_files(&crate_dir.join("src"), &mut files);
    files
}

fn collect_rs_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()));

    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("failed to read directory entry: {err}"));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|err| panic!("failed to inspect {}: {err}", path.display()));

        if file_type.is_dir() {
            collect_rs_files(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}
