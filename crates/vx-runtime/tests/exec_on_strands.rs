//! `exec(command) with { observer }` end-to-end — the language surface of the exec
//! spine, returning the observer's **real value**. A real `.vx` program is compiled
//! through the bundled front-end, then evaluated on a strand: eval hits the
//! `with`-decorated `exec`, projects the command to its argv, bundles the observer
//! closure's static reachable closure, and dispatches the `observe` RPC to an
//! executor **over vox** (`local_observer`). The strand parks; the boss drives the
//! call (the executor runs the real process, builds a `Run`, evaluates the observer
//! against it, and **deep-forces the result into a value**); the strand resumes, and
//! `exec(…)` evaluates to that value — reconstructed from the wire.
//!
//! So `` `$cmd` with { observer: (run) => run.ok } `` returns the actual
//! `Exit` struct that crossed back, not a placeholder. (`observe_on_strands` drives
//! `dispatch_observe` directly; `observe`'s own test asserts the crossed `WireValue`.)

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Value, eval_body_with_exec, tool_value,
};
use vx_runtime::observe::local_observer;
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

/// Compile `` `$cmd` with { observer: (run) => run.ok } ``, run it on a
/// strand against a local executor running `binary`, and return the strand outcome.
/// On success the observer's value is the `Exit` struct — verified worker-side (a
/// `Value` can't cross the strand boundary), so a regression to `Unit` fails here.
fn exec_outcome(binary: &str) -> Option<Outcome> {
    let fe = compile_source(
        "fn t(cmd: Command) -> Exit ( `$cmd` with { observer: (run) => run.ok } )",
        "t",
    )
    .expect("exec program compiles");
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
    // Establish the observe client on the scheduler's runtime so the boss drives it.
    let client = s.block_on(local_observer());
    let cid = s.register_observe_client(client);

    // Move the owned (Send) program + env into the strand body; the Rc-bearing
    // command/eval state is built worker-side and never crosses a thread.
    let program = fe.program;
    let env = fe.env;
    let binary = binary.to_string();
    let strand = s.spawn(ResourceId(0), move || {
        // A `cc`-grammar command whose binary is the real `true`/`false`. The
        // `` `$cmd` `` literal carries the grammar coordinate forward and applies no
        // args, so `exec` runs the bare `binary`.
        let cmd = tool_value("@vixen/grammar-cc", "cc", &binary);
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::Off, cid) {
            // `run.ok` is the `Exit` struct; it crossed back over vox and `exec`
            // evaluates to it. Confirm the shape here — `Value` is `!Send` and can't
            // leave the strand, and a regression to `Value::Unit` trips this.
            EvalOutcome::Clean(Value::Struct(fields)) => {
                let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                if names == ["stdout", "stderr"] {
                    Outcome::Clean
                } else {
                    Outcome::Poisoned(format!("exec returned a struct with fields {names:?}"))
                }
            }
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("exec returned {other:?}, not the Exit struct"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    s.outcome(strand)
}

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

/// `true` exits 0 → the observer's `run.ok` forces to an `Exit` → that whole struct
/// crosses back and `exec(…)` evaluates to it (verified worker-side). The whole path
/// — `with`-hook, command projection, observer bundling, observe + value crossing
/// over vox on a strand — ran end to end.
#[test]
fn exec_over_vox_returns_the_observers_exit_value() {
    assert_eq!(
        exec_outcome("true"),
        Some(Outcome::Clean),
        "exit 0 → exec returns the Exit struct the observer produced"
    );
}

/// `false` exits 1 → the observer's `run.ok` poisons → that poison crosses back from
/// the executor (`Observed::Poisoned`) and detonates the strand (I6 across vox).
#[test]
fn exec_over_vox_carries_a_failed_process_back_as_a_poison() {
    match exec_outcome("false") {
        Some(Outcome::Poisoned(reason)) => {
            assert!(
                !reason.is_empty(),
                "the failure reason crosses back: {reason:?}"
            )
        }
        other => panic!("exit 1 → run.ok poisons → exec poisons, got {other:?}"),
    }
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
fn identical_backtick_exec_boundaries_share_the_l1_entry() {
    let marker = exec_cache_marker("same");
    let _ = std::fs::remove_file(&marker);
    let script = format!("printf x >> {marker}; printf CONTENT > out.txt");
    let fe = compile_source(
        "fn t(cmd: Command, script: String) ( \
           let first = `$cmd -c ${script}`; \
           let second = `$cmd -c ${script}`; \
           assert_eq(first.file(\"out.txt\"), second.file(\"out.txt\")) )",
        "t",
    )
    .expect("exec cache program compiles");
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
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected both cached exec results, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(
        s.outcome(strand),
        Some(Outcome::Clean),
        "both demanded invocation nodes resolve"
    );

    let count = std::fs::read_to_string(&marker).expect("exec cache marker file");
    let _ = std::fs::remove_file(&marker);
    assert_eq!(
        count, "x",
        "the second identical backtick exec must reuse the L1 result"
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
