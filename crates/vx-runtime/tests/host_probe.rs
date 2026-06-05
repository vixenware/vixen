//! The `Host` capability as an ordinary struct: a probe-shaped fn
//! `probe(host: Host) -> String ( host.which("sh") )` receives a `Host` struct
//! value (`{ os, arch }`), resolves `sh` on `$PATH`, and returns its absolute
//! path. `host.which` is a name-based method dispatch on a `Value::Struct` (no
//! `Value::Host` variant, no opaque holder), and a `host.os` field read returns
//! the compiled-in target OS — proving `Host` is just a struct.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vx_ir::item::ItemKind;
use vx_runtime::cache::{RuntimeCacheBackend, RuntimeCacheRecordKey};
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{KernelMode, Outcome, Value, eval_body, eval_body_with_cache_backend};
use vx_runtime::host::local_host_value;

static NEXT_MARKER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Default)]
struct RecordingCacheBackend {
    records: Arc<Mutex<HashMap<(String, String), String>>>,
}

impl RuntimeCacheBackend for RecordingCacheBackend {
    fn get_record(&self, key: &RuntimeCacheRecordKey) -> Option<String> {
        self.records
            .lock()
            .unwrap()
            .get(&(key.namespace().to_string(), key.input_digest().to_string()))
            .cloned()
    }

    fn put_record(&self, key: &RuntimeCacheRecordKey, record: &str) {
        self.records.lock().unwrap().insert(
            (key.namespace().to_string(), key.input_digest().to_string()),
            record.to_string(),
        );
    }
}

/// Find the body of the single fn named `name` in a compiled program.
fn fn_body(fe: &vx_runtime::compile::FrontEnd, name: &str) -> vx_ir::ids::BodyId {
    fe.program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == name => Some(f.body),
            _ => None,
        })
        .unwrap_or_else(|| panic!("fn {name}"))
}

#[test]
fn host_which_resolves_a_tool() {
    let fe = compile_source(
        "fn probe(host: Host) -> String ( host.which(\"sh\") )",
        "probe",
    )
    .expect("host probe program compiles");
    let body = fn_body(&fe, "probe");

    // The sole argument is the host capability — an ordinary `{ os, arch }` struct
    // value, no dedicated `Value` variant. `sh` is always on `$PATH`.
    let outcome = eval_body(
        &fe.program,
        &fe.env,
        body,
        vec![local_host_value()],
        KernelMode::Off,
    );

    match outcome {
        // `which` returns the resolved absolute path; it ends in `sh`, proving the
        // `$PATH` lookup ran against the real FS.
        Outcome::Clean(Value::Str(s)) => {
            assert!(
                s.contains("sh"),
                "resolved path of `sh` must mention it, got {s:?}"
            );
        }
        Outcome::Clean(other) => panic!("expected the resolved path as a Str, got {other:?}"),
        Outcome::Poisoned { reason, .. } => panic!("host probe poisoned: {reason}"),
    }
}

#[test]
fn host_os_field_reads() {
    let fe = compile_source("fn f(host: Host) -> String ( host.os )", "f")
        .expect("host field program compiles");
    let body = fn_body(&fe, "f");

    let outcome = eval_body(
        &fe.program,
        &fe.env,
        body,
        vec![local_host_value()],
        KernelMode::Off,
    );

    match outcome {
        // `host.os` is a plain struct field read — the compiled-in target OS.
        Outcome::Clean(Value::Str(s)) => {
            assert!(!s.is_empty(), "host.os must be a non-empty target OS name");
        }
        Outcome::Clean(other) => panic!("expected the host OS as a Str, got {other:?}"),
        Outcome::Poisoned { reason, .. } => panic!("host.os read poisoned: {reason}"),
    }
}

/// `host.exec` runs a discovered tool and — *implying `host.hash`* — returns the
/// `blake3:…` fingerprint of the binary alongside its captured output. `sh` is on
/// every `$PATH`, so this exercises the real spawn + hash without assuming a
/// toolchain.
#[test]
fn host_exec_runs_a_tool_and_fingerprints_the_binary() {
    let run = |src: &str| -> Outcome {
        let fe = compile_source(src, "probe").expect("host exec program compiles");
        let body = fn_body(&fe, "probe");
        eval_body(
            &fe.program,
            &fe.env,
            body,
            vec![local_host_value()],
            KernelMode::Off,
        )
    };

    // The captured output crosses back: `sh -c 'printf hi'` writes `hi` to stdout.
    match run("fn probe(host: Host) -> String ( \
           host.exec([host.which(\"sh\"), \"-c\", \"printf hi\"]).stdout )")
    {
        Outcome::Clean(Value::Str(s)) => assert_eq!(&*s, "hi", "sh's stdout crosses back"),
        other => panic!("expected stdout `hi`, got {other:?}"),
    }

    // exec implies hash: the binary that ran is fingerprinted (blake3 of sh's bytes).
    match run("fn probe(host: Host) -> String ( \
           host.exec([host.which(\"sh\"), \"-c\", \"true\"]).fingerprint )")
    {
        Outcome::Clean(Value::Str(s)) => assert!(
            s.starts_with("blake3:") && s.len() > "blake3:".len(),
            "fingerprint is a blake3 digest, got {s:?}"
        ),
        other => panic!("expected a blake3 fingerprint, got {other:?}"),
    }
}

#[test]
fn host_exec_resolves_bare_program_names_for_fingerprinting() {
    let fe = compile_source(
        "fn probe(host: Host) -> String ( host.exec([\"sh\", \"-c\", \"printf hi\"]).stdout )",
        "probe",
    )
    .expect("host bare exec program compiles");
    let body = fn_body(&fe, "probe");

    let outcome = eval_body(
        &fe.program,
        &fe.env,
        body,
        vec![local_host_value()],
        KernelMode::Off,
    );

    match outcome {
        Outcome::Clean(Value::Str(s)) => assert_eq!(&*s, "hi", "bare sh stdout crosses back"),
        other => panic!("expected stdout `hi`, got {other:?}"),
    }
}

#[test]
fn host_exec_l2_cache_survives_fresh_graph_eval() {
    let marker = std::env::temp_dir().join(format!(
        "vixen-host-exec-l2-marker-{}-{}",
        std::process::id(),
        NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
    ));
    let marker = marker.to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&marker);
    let script = format!("printf x >> {marker}; cat {marker}");
    let fe = compile_source(
        &format!(
            "fn probe(host: Host) -> String ( \
             let script = \"{script}\"; \
             host.exec([\"sh\", \"-c\", script]).stdout )"
        ),
        "probe",
    )
    .expect("host exec l2 program compiles");
    let body = fn_body(&fe, "probe");
    let backend = RecordingCacheBackend::default();

    let run_once = || {
        eval_body_with_cache_backend(
            &fe.program,
            &fe.env,
            body,
            vec![local_host_value()],
            KernelMode::Off,
            backend.clone(),
        )
    };

    match run_once() {
        Outcome::Clean(Value::Str(s)) => assert_eq!(&*s, "x", "first host.exec run spawns"),
        other => panic!("expected first stdout `x`, got {other:?}"),
    }
    match run_once() {
        Outcome::Clean(Value::Str(s)) => assert_eq!(&*s, "x", "second run uses L2"),
        other => panic!("expected cached stdout `x`, got {other:?}"),
    }

    let count = std::fs::read_to_string(&marker).expect("host.exec l2 marker file");
    let _ = std::fs::remove_file(&marker);
    assert_eq!(
        count, "x",
        "a fresh graph evaluation with the same backend must not spawn again"
    );
}

#[test]
fn host_exec_uses_input_keyed_l1_cache() {
    let marker = std::env::temp_dir().join(format!(
        "vixen-host-exec-cache-{}-{}",
        std::process::id(),
        NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
    ));
    let marker = marker.to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&marker);
    let script = format!("printf x >> {marker}; cat {marker}");

    let fe = compile_source(
        &format!(
            "fn probe(host: Host) -> Bool ( \
             let script = \"{script}\"; \
             let first = host.exec([host.which(\"sh\"), \"-c\", script]).stdout; \
             let second = host.exec([host.which(\"sh\"), \"-c\", script]).stdout; \
             (first == \"x\") && (second == \"x\") )"
        ),
        "probe",
    )
    .expect("host exec cache program compiles");
    let body = fn_body(&fe, "probe");

    let outcome = eval_body(
        &fe.program,
        &fe.env,
        body,
        vec![local_host_value()],
        KernelMode::Off,
    );

    match outcome {
        Outcome::Clean(Value::Bool(true)) => {}
        other => panic!("identical host.exec boundaries should share the cached output: {other:?}"),
    }

    let count = std::fs::read_to_string(&marker).expect("host.exec marker file");
    let _ = std::fs::remove_file(&marker);
    assert_eq!(
        count, "x",
        "the second identical host.exec boundary must not spawn again"
    );
}
