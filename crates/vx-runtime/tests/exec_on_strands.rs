//! Scratch-backed exec cache and cold-demand compatibility coverage.
//!
//! These tests still use `local_observer()`, so they exercise the quarantined
//! scratch observe service. Production-path value crossing now lives in
//! `vx-runner` runner-backed proofs; this file remains only until the cache-key
//! cases have runner-backed replacements or explicit unsupported states.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Value, eval_body_with_exec,
    eval_body_with_exec_and_cache_dir, tool_value,
};
use vx_runtime::observe::local_observer;
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

fn cold_exec_outcome(binary: &str) -> Option<Outcome> {
    let fe = compile_source(
        "fn t(cmd: Command) -> U64 ( \
           let pair = ( `$cmd` with { observer: (run) => run.ok }, 42 ); \
           pair[1] )",
        "t",
    )
    .expect("exec demand program compiles");
    let entry = fe
        .program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == "t" => Some(f.body),
            _ => None,
        })
        .expect("fn t");

    let s = Scheduler::new(&[4]);
    let cid = 0;

    let program = fe.program;
    let env = fe.env;
    let binary = binary.to_string();
    let strand = s.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", &binary);
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::Off, cid) {
            EvalOutcome::Clean(Value::U64(42)) => Outcome::Clean,
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected the demanded sibling 42, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    s.outcome(strand)
}

fn exec_cache_marker(name: &str) -> String {
    static NEXT_MARKER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir()
        .join(format!(
            "vx-exec-cache-{name}-{}-{}",
            std::process::id(),
            NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
        ))
        .to_string_lossy()
        .into_owned()
}

fn exec_cache_dir(name: &str) -> PathBuf {
    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "vx-exec-l2-{name}-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ))
}

/// The cold tuple field is a real backtick exec whose observer would poison for
/// `false`, but the caller demands only the second field. A recursive eager walk
/// runs the command and fails; graph demand leaves it dark.
#[test]
fn undemanded_exec_in_tuple_does_not_dispatch() {
    assert_eq!(
        cold_exec_outcome("false"),
        Some(Outcome::Clean),
        "only pair[1] is demanded, so the failing exec in pair[0] must stay cold"
    );
}

#[cfg(unix)]
#[test]
fn backtick_exec_l2_cache_survives_fresh_graph_eval() {
    use std::os::unix::fs::PermissionsExt;

    let binary = exec_cache_marker("l2-binary");
    let marker = exec_cache_marker("l2-marker");
    let cache_dir = exec_cache_dir("backtick");
    let _ = std::fs::remove_file(&binary);
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_dir_all(&cache_dir);
    std::fs::write(
        &binary,
        format!("#!/bin/sh\nprintf x >> {marker}\nprintf same\n"),
    )
    .expect("write l2 executable");
    let mut perms = std::fs::metadata(&binary)
        .expect("l2 executable metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&binary, perms).expect("make l2 executable runnable");

    fn run_once(binary: &str, cache_dir: &Path) -> Option<Outcome> {
        let fe = compile_source(
            "fn t(cmd: Command) ( \
               let run = `$cmd` with { observer: (run) => run.ok }; \
               assert_eq(run.stdout, run.stdout) )",
            "t",
        )
        .expect("exec l2 cache program compiles");
        let entry = fe
            .program
            .arena
            .items
            .iter()
            .find_map(|it| match &it.kind {
                ItemKind::Function(f) if f.name == "t" => Some(f.body),
                _ => None,
            })
            .expect("fn t");

        let s = Scheduler::new(&[4]);
        let client = s.block_on(local_observer());
        let cid = s.register_observe_client(client);

        let program = fe.program;
        let env = fe.env;
        let binary = binary.to_string();
        let cache_dir = cache_dir.to_path_buf();
        let strand = s.spawn(ResourceId(0), move || {
            let cmd = tool_value("@vixen/grammar-cc", "cc", &binary);
            match eval_body_with_exec_and_cache_dir(
                &program,
                &env,
                entry,
                vec![cmd],
                KernelMode::Off,
                cid,
                &cache_dir,
            ) {
                EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
                EvalOutcome::Clean(other) => Outcome::Poisoned(format!(
                    "expected cached exec assertion to return unit, got {other:?}"
                )),
                EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
            }
        });
        s.demand(strand);
        s.run();
        s.outcome(strand)
    }

    assert_eq!(
        run_once(&binary, &cache_dir),
        Some(Outcome::Clean),
        "first demanded backtick run should spawn and populate L2"
    );
    assert_eq!(
        run_once(&binary, &cache_dir),
        Some(Outcome::Clean),
        "fresh graph eval should reuse the persisted observer value"
    );
    let count = std::fs::read_to_string(&marker).expect("exec l2 marker file");
    let _ = std::fs::remove_file(&binary);
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_dir_all(&cache_dir);
    assert_eq!(
        count, "x",
        "a fresh graph evaluation with the same L2 dir must not dispatch observe again"
    );
}
