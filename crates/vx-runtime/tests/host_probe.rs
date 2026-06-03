//! The `Host` capability, end to end through the generic opaque holder: a
//! probe-shaped fn `probe(host: Host) -> Blob ( host.hash(host.which("sh")) )`
//! receives a `Host` as a `Value::Opaque(HostHandle)`, resolves `sh` on `$PATH`,
//! and hashes its binary. Proves method dispatch downcasts the opaque value (no
//! `Value::Host` variant) and that `which`/`hash` run against the real FS.

use std::rc::Rc;

use vx_ir::compile::compile_source;
use vx_ir::item::ItemKind;
use vx_runtime::eval::{KernelMode, Outcome, Value, eval_body};
use vx_runtime::host::HostHandle;

#[test]
fn host_probe_hashes_a_resolved_tool() {
    let fe = compile_source(
        "fn probe(host: Host) -> Blob ( host.hash(host.which(\"sh\")) )",
        "probe",
    )
    .expect("host probe program compiles");
    let body = fe
        .program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == "probe" => Some(f.body),
            _ => None,
        })
        .expect("fn probe");

    // The sole argument is the host capability, held generically — no dedicated
    // `Value` variant. `sh` is always on `$PATH`.
    let host = Value::Opaque(Rc::new(HostHandle::local()));
    let outcome = eval_body(&fe.program, &fe.env, body, vec![host], KernelMode::Off);

    match outcome {
        // The blake3 hex of `sh`'s binary, rendered lossily as a `Str` (the Blob
        // convention). Non-empty proves which→hash ran end to end.
        Outcome::Clean(Value::Str(s)) => {
            assert!(!s.is_empty(), "blake3 hex of `sh` must be non-empty");
        }
        Outcome::Clean(other) => panic!("expected a Blob-as-Str, got {other:?}"),
        Outcome::Poisoned { reason, .. } => panic!("host probe poisoned: {reason}"),
    }
}
