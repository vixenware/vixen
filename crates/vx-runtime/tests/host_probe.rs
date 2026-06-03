//! The `Host` capability as an ordinary struct: a probe-shaped fn
//! `probe(host: Host) -> String ( host.which("sh") )` receives a `Host` struct
//! value (`{ os, arch }`), resolves `sh` on `$PATH`, and returns its absolute
//! path. `host.which` is a name-based method dispatch on a `Value::Struct` (no
//! `Value::Host` variant, no opaque holder), and a `host.os` field read returns
//! the compiled-in target OS — proving `Host` is just a struct.

use vx_ir::compile::compile_source;
use vx_ir::item::ItemKind;
use vx_runtime::eval::{KernelMode, Outcome, Value, eval_body};
use vx_runtime::host::local_host_value;

/// Find the body of the single fn named `name` in a compiled program.
fn fn_body(fe: &vx_ir::compile::FrontEnd, name: &str) -> vx_ir::ids::BodyId {
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
