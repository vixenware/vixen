//! The eval↔remote join: a real `.vx` program forces a file out of a **remote
//! producing tree** over vox, on a strand. `tree.file("path")` in eval calls
//! `force_remote` (the consumer half of `remote.rs`) — if the path isn't produced
//! yet the strand **parks** while the boss drives the vox `force()`, then resumes
//! with the bytes; a path the producer never makes poisons.
//!
//! This is the join `remote.rs` flagged as missing ("eval's own `force` calling
//! `force_remote` … comes with exec"). Here a producing tree is stood up directly
//! (`remote::channel`) and bound as the program's `Tree` argument; making `exec`
//! *produce* such a handle (the executor serving its outputs) is the next piece.

use std::thread;

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{KernelMode, Outcome as EvalOutcome, Value, eval_body};
use vx_runtime::remote;
use vx_runtime::strand::{HandleId, Outcome, ResourceId, Scheduler, StrandId};

/// Spawn `fn t(tree: Tree) -> Blob ( tree.file("out") )` as a strand against the
/// remote tree `hid`: `Clean` iff `tree.file("out")` forces to `want`, else
/// `Poisoned` (carrying eval's reason — e.g. an absent part).
fn force_out(s: &Scheduler, hid: HandleId, want: &'static str) -> StrandId {
    let fe = compile_source("fn t(tree: Tree) -> Blob ( tree.file(\"out\") )", "t")
        .expect("exec/tree program compiles");
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
    let program = fe.program;
    let env = fe.env;
    s.spawn(ResourceId(0), move || {
        // The Rc-bearing tree value is built worker-side; eval forces `"out"` over vox.
        match eval_body(
            &program,
            &env,
            entry,
            vec![Value::Tree(hid)],
            KernelMode::Off,
        ) {
            EvalOutcome::Clean(Value::Str(s)) if &*s == want => Outcome::Clean,
            EvalOutcome::Clean(v) => Outcome::Poisoned(format!("wrong value: {v:?}")),
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    })
}

/// The path is already produced → `tree.file("out")` forces it on the first try,
/// over vox, and `exec`/eval evaluates to its bytes.
#[test]
fn eval_forces_a_file_already_in_the_remote_tree() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);
    producer.write_path("out", b"CONTENT");

    let strand = force_out(&s, hid, "CONTENT");
    s.demand(strand);
    s.run();
    assert_eq!(s.outcome(strand), Some(Outcome::Clean));
    drop(producer); // hold the producer (its vox connection) until the run is done
}

/// The path isn't produced yet: the strand parks inside `tree.file` (the boss drives
/// the vox `force`), the producer writes it from another thread *while* `run()` is
/// driving, and eval resumes with the bytes.
#[test]
fn eval_parks_until_the_remote_file_is_produced() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);

    let strand = force_out(&s, hid, "LATE");
    s.demand(strand);
    let writer = thread::spawn(move || {
        producer.write_path("out", b"LATE");
        producer // keep the producer (and its vox connection) alive until joined
    });
    s.run();
    let _ = writer.join();
    assert_eq!(s.outcome(strand), Some(Outcome::Clean));
}

/// The producer finishes without ever making `out` → the forced part comes back
/// `Absent`, and `tree.file` poisons through eval (I6 across executors).
#[test]
fn eval_poisons_when_the_remote_file_is_never_produced() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);

    let strand = force_out(&s, hid, "unused");
    s.demand(strand);
    let writer = thread::spawn(move || {
        producer.finish();
        producer
    });
    s.run();
    let _ = writer.join();
    match s.outcome(strand) {
        Some(Outcome::Poisoned(why)) => {
            assert!(
                why.contains("absent"),
                "expected an absent-part poison: {why}"
            )
        }
        other => panic!("expected a poison, got {other:?}"),
    }
}
