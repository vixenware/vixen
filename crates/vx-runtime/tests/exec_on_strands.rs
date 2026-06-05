//! Scratch-backed exec cache and cold-demand compatibility coverage.
//!
//! These tests still use `local_observer()`, so they exercise the quarantined
//! scratch observe service. Production-path value crossing now lives in
//! `vx-runner` runner-backed proofs; this file remains only until the cache-key
//! cases have runner-backed replacements or explicit unsupported states.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Thunk, Value, eval_body_with_exec,
    eval_body_with_exec_and_cache_dir, grammar_coord_value, tool_value,
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

fn tool_value_with_fingerprint(
    package: &str,
    command: &str,
    binary: &str,
    fingerprint: &str,
) -> Value {
    Value::Struct(vec![
        (
            "command".to_string(),
            Thunk::done(grammar_coord_value(package, command)),
        ),
        (
            "binary".to_string(),
            Thunk::done(Value::Str(Rc::from(binary))),
        ),
        (
            "fingerprint".to_string(),
            Thunk::done(Value::Str(Rc::from(fingerprint))),
        ),
    ])
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

#[test]
fn writable_roots_are_part_of_the_backtick_exec_l1_key() {
    let marker = exec_cache_marker("writable");
    let _ = std::fs::remove_file(&marker);
    let script = format!("printf x >> {marker}; printf CONTENT > out.txt");
    let fe = compile_source(
        "fn t(cmd: Command, script: String) ( \
           let first = `$cmd -c ${script}` with { writable: [\"out-a\"] }; \
           let second = `$cmd -c ${script}` with { writable: [\"out-b\"] }; \
           assert_eq(first.file(\"out.txt\"), second.file(\"out.txt\")) )",
        "t",
    )
    .expect("exec writable-cache program compiles");
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
    let strand = s.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "sh");
        let script = Value::Str(Rc::from(script.as_str()));
        match eval_body_with_exec(
            &program,
            &env,
            entry,
            vec![cmd, script],
            KernelMode::Off,
            cid,
        ) {
            EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
            EvalOutcome::Clean(other) => Outcome::Poisoned(format!(
                "expected both writable-distinct exec results, got {other:?}"
            )),
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(
        s.outcome(strand),
        Some(Outcome::Clean),
        "both writable-distinct invocation nodes resolve"
    );

    let count = std::fs::read_to_string(&marker).expect("exec writable marker file");
    let _ = std::fs::remove_file(&marker);
    assert_eq!(
        count, "xx",
        "different writable roots must not alias in the backtick exec L1"
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

#[cfg(unix)]
#[test]
fn declared_tool_fingerprint_is_part_of_the_backtick_exec_key() {
    use std::os::unix::fs::PermissionsExt;

    let binary = exec_cache_marker("declared-fingerprint-binary");
    let marker = exec_cache_marker("declared-fingerprint-marker");
    let _ = std::fs::remove_file(&binary);
    let _ = std::fs::remove_file(&marker);
    std::fs::write(
        &binary,
        format!("#!/bin/sh\nprintf x >> {marker}\nprintf same\n"),
    )
    .expect("write declared-fingerprint executable");
    let mut perms = std::fs::metadata(&binary)
        .expect("declared-fingerprint executable metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&binary, perms)
        .expect("make declared-fingerprint executable runnable");

    let fe = compile_source(
        "fn t(first_cmd: Command, second_cmd: Command) ( \
           let first = `$first_cmd` with { observer: (run) => run.ok }; \
           let second = `$second_cmd` with { observer: (run) => run.ok }; \
           assert_eq(first.stdout, second.stdout) )",
        "t",
    )
    .expect("exec declared fingerprint cache program compiles");
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
    let first_binary = binary.clone();
    let second_binary = binary.clone();
    let strand = s.spawn(ResourceId(0), move || {
        let first =
            tool_value_with_fingerprint("@vixen/grammar-cc", "cc", &first_binary, "declared:first");
        let second = tool_value_with_fingerprint(
            "@vixen/grammar-cc",
            "cc",
            &second_binary,
            "declared:second",
        );
        match eval_body_with_exec(
            &program,
            &env,
            entry,
            vec![first, second],
            KernelMode::Off,
            cid,
        ) {
            EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
            EvalOutcome::Clean(other) => Outcome::Poisoned(format!(
                "expected both declared-fingerprint exec results, got {other:?}"
            )),
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(
        s.outcome(strand),
        Some(Outcome::Clean),
        "both declared-fingerprint invocation nodes resolve"
    );

    let count = std::fs::read_to_string(&marker).expect("exec declared fingerprint marker file");
    let _ = std::fs::remove_file(&binary);
    let _ = std::fs::remove_file(&marker);
    assert_eq!(
        count, "xx",
        "different declared tool fingerprints must not share the backtick exec cache entry"
    );
}

#[cfg(unix)]
#[test]
fn backtick_exec_l1_keys_the_binary_fingerprint() {
    use std::os::unix::fs::PermissionsExt;

    let link = exec_cache_marker("binary-fingerprint");
    let first_path = format!("{link}.first");
    let second_path = format!("{link}.second");
    let marker = format!("{link}.marker");
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_file(&first_path);
    let _ = std::fs::remove_file(&second_path);
    let _ = std::fs::remove_file(&marker);
    std::fs::write(
        &first_path,
        format!("#!/bin/sh\nln -sf {second_path} {link}\nprintf a >> {marker}\nprintf same\n"),
    )
    .expect("write first executable");
    std::fs::write(
        &second_path,
        format!("#!/bin/sh\nprintf b >> {marker}\nprintf same\n"),
    )
    .expect("write second executable");
    for path in [&first_path, &second_path] {
        let mut perms = std::fs::metadata(path)
            .expect("executable metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("make executable runnable");
    }
    std::os::unix::fs::symlink(&first_path, &link).expect("link executable path to v1");

    let fe = compile_source(
        "fn t(cmd: Command) ( \
           let first = `$cmd` with { observer: (run) => run.ok }; \
           let second = `$cmd` with { observer: (run) => run.ok }; \
           assert_eq(first.stdout, second.stdout) )",
        "t",
    )
    .expect("exec binary fingerprint cache program compiles");
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
    let binary = link.clone();
    let strand = s.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", &binary);
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::Off, cid) {
            EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected both executable versions, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(
        s.outcome(strand),
        Some(Outcome::Clean),
        "changing the executable at the same path must miss the backtick exec L1"
    );
    let marker_contents = std::fs::read_to_string(&marker).expect("binary cache marker file");
    assert_eq!(
        marker_contents, "ab",
        "the second executable version must run instead of reusing the path-keyed L1 entry"
    );
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_file(&first_path);
    let _ = std::fs::remove_file(&second_path);
    let _ = std::fs::remove_file(&marker);
}

#[test]
fn observer_identity_is_part_of_the_backtick_exec_l1_key() {
    let marker = exec_cache_marker("observer");
    let _ = std::fs::remove_file(&marker);
    let script = format!("printf x >> {marker}; printf out");
    let fe = compile_source(
        "fn id(exit: Exit) -> Exit ( exit ) \
         fn t(cmd: Command, script: String) ( \
           let direct = `$cmd -c ${script}` with { observer: (run) => run.ok }; \
           let wrapped = `$cmd -c ${script}` with { observer: (run) => id(run.ok) }; \
           assert_eq(direct.stdout, wrapped.stdout) )",
        "t",
    )
    .expect("exec cache observer program compiles");
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
    let strand = s.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", "sh");
        let script = Value::Str(Rc::from(script.as_str()));
        match eval_body_with_exec(
            &program,
            &env,
            entry,
            vec![cmd, script],
            KernelMode::Off,
            cid,
        ) {
            EvalOutcome::Clean(Value::Unit) => Outcome::Clean,
            EvalOutcome::Clean(other) => Outcome::Poisoned(format!(
                "expected observer-specific exec results, got {other:?}"
            )),
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(
        s.outcome(strand),
        Some(Outcome::Clean),
        "both observer-specific invocation nodes resolve"
    );

    let count = std::fs::read_to_string(&marker).expect("exec cache observer marker file");
    let _ = std::fs::remove_file(&marker);
    assert_eq!(
        count, "xx",
        "different observer bundles must not alias in the backtick exec L1"
    );
}
