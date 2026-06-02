//! The exec value model — building a `Run` value from a finished process.
//!
//! `Run` is a **plain struct** whose faces are native-backed (the intrinsic
//! mechanism in `eval`): forcing `.ok` runs a success gate (→ an `Exit`, or poisons
//! with a `ProcError`); `.stream` is the live face. Field access (`run.ok`) is
//! ordinary — only the field's *value* is Rust, so there is no `Value::Run` variant
//! and no special-casing in eval.
//!
//! #1b builds these synthetically to exercise the gate; #2 feeds it real `run_exec`
//! results. The byte/tree/duration field representations and the producing `out`
//! handle fill in with the streaming work.

use std::rc::Rc;

use crate::eval::{Poison, Thunk, Value};

/// A finished process — the raw result the executor's `run_exec` produces.
#[derive(Debug, Clone)]
pub struct ProcOutcome {
    /// Exit status: the code, or `128 + signal` if killed by a signal.
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Build a `Run` value — a struct whose `ok`/`stream` faces are native intrinsics.
pub fn build(outcome: ProcOutcome) -> Value {
    let o = Rc::new(outcome);
    let for_ok = Rc::clone(&o);
    Value::Struct(vec![
        // `.ok` — settled, success-gated: forcing it runs the gate.
        ("ok".to_string(), Thunk::native(move || ok_gate(&for_ok))),
        // `.stream` — the live face (stub until the producing-handle work).
        ("stream".to_string(), Thunk::native(|| Ok(live_face()))),
    ])
}

/// The success gate: exit 0 → an `Exit`; otherwise poison with a `ProcError` (so a
/// later `run.ok?` catches `Result[Exit, ProcError]`).
fn ok_gate(o: &ProcOutcome) -> Result<Value, Poison> {
    if o.exit_code == 0 {
        Ok(exit_value(o))
    } else {
        Err(Poison {
            err: proc_error_value(o),
            context: Default::default(),
        })
    }
}

/// An `Exit` struct value. Carried minimally for now — the byte/tree/duration value
/// representations land with the streaming + handle work.
fn exit_value(o: &ProcOutcome) -> Value {
    Value::Struct(vec![
        ("stdout".to_string(), Thunk::done(bytes_value(&o.stdout))),
        ("stderr".to_string(), Thunk::done(bytes_value(&o.stderr))),
    ])
}

/// A `ProcError` struct value — the domain error of a failed run. `cause` is the
/// meaningful part (`Exited(code)`); the remaining fields fill in as the model grows.
fn proc_error_value(o: &ProcOutcome) -> Value {
    Value::Struct(vec![(
        "cause".to_string(),
        Thunk::done(Value::Variant {
            tag: "Exited".to_string(),
            fields: vec![("0".to_string(), Thunk::done(Value::U64(o.exit_code as u64)))],
        }),
    )])
}

fn live_face() -> Value {
    // The live face (`stdout`/`stderr` streams + the producing `out` tree) is the
    // streaming work; an empty struct stands in until then.
    Value::Struct(Vec::new())
}

/// Bytes as a value — provisional, rendered lossily as a string until a real `Bytes`
/// value lands.
fn bytes_value(b: &[u8]) -> Value {
    Value::Str(Rc::from(String::from_utf8_lossy(b).as_ref()))
}
