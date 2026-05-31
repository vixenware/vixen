//! The remote-part bridge end to end: a strand forces a part of a remote handle,
//! and if it isn't produced yet the strand **parks** (freeing its worker) while the
//! boss drives the vox `force()` on its runtime, then **resumes** when the part
//! lands. This is the cross-executor demand-pull — over the public API, with a real
//! vox connection (`remote::channel`).
//!
//! Outcomes here are timing-independent: a producer that writes a path writes it
//! unconditionally, so the consuming strand always ends up with those bytes — only
//! *whether* it observably parked first depends on scheduling, which we don't assert.

use vx_runtime::remote::{self, Part, PartKey};
use vx_runtime::strand::{HandleId, Outcome, ResourceId, Scheduler, StrandId, force_remote};

/// A strand that forces `path` of `hid` and checks the bytes are `want` — `Clean`
/// iff they match, else `Poisoned` (carrying an `Absent` producer's reason).
fn expect_bytes(s: &Scheduler, hid: HandleId, path: &str, want: &'static [u8]) -> StrandId {
    let path = path.to_string();
    s.spawn(ResourceId(0), move || {
        let part = force_remote(hid, PartKey::TreePath(path));
        match &*part {
            Part::Bytes(b) if b == want => Outcome::Clean,
            Part::Bytes(_) => Outcome::Poisoned("wrong bytes".into()),
            Part::Absent(why) => Outcome::Poisoned(format!("absent: {why}")),
        }
    })
}

/// A part already produced resolves on the first force — no real waiting.
#[test]
fn forces_a_part_already_produced() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);
    producer.write_path("libA.rmeta", b"META");
    let strand = expect_bytes(&s, hid, "libA.rmeta", b"META");
    s.demand(strand);
    s.run();
    assert_eq!(s.outcome(strand), Some(Outcome::Clean));
}

/// A part not yet produced: the strand parks (`AwaitingRemotePart`), the producer
/// writes it from another thread *while* `run()` drives the force, and the strand
/// resumes with the bytes.
#[test]
fn parks_until_the_part_is_produced_then_resumes() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);
    let strand = expect_bytes(&s, hid, "libA.rlib", b"LIB");
    s.demand(strand);
    let writer = std::thread::spawn(move || {
        producer.write_path("libA.rlib", b"LIB");
        producer // keep the producer (and its vox connection) alive until joined
    });
    s.run();
    let _ = writer.join();
    assert_eq!(s.outcome(strand), Some(Outcome::Clean));
}

/// A part the producer never makes (it `finish`es first) comes back `Absent` — the
/// strand detonates with the producer's reason (I6 across executors).
#[test]
fn a_never_produced_part_poisons_the_strand() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);
    let strand = expect_bytes(&s, hid, "never", b"unused");
    s.demand(strand);
    let writer = std::thread::spawn(move || {
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
        other => panic!("expected poison, got {other:?}"),
    }
}

/// Cargo pipelining inside one strand: it forces `rmeta` (already produced, resolves
/// at once) then `rlib` (parks until produced) — progressive resolution of one
/// remote tree, not an atomic output.
#[test]
fn a_strand_pipelines_rmeta_before_rlib() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);
    producer.write_path("libA.rmeta", b"META"); // available now; rlib comes later
    let strand = s.spawn(ResourceId(0), move || {
        let rmeta = force_remote(hid, PartKey::TreePath("libA.rmeta".into()));
        let rlib = force_remote(hid, PartKey::TreePath("libA.rlib".into()));
        match (&*rmeta, &*rlib) {
            (Part::Bytes(m), Part::Bytes(l)) if m == b"META" && l == b"LIB" => Outcome::Clean,
            _ => Outcome::Poisoned("pipelining mismatch".into()),
        }
    });
    s.demand(strand);
    let writer = std::thread::spawn(move || {
        producer.write_path("libA.rlib", b"LIB");
        producer
    });
    s.run();
    let _ = writer.join();
    assert_eq!(s.outcome(strand), Some(Outcome::Clean));
}

/// Two strands demanding the *same* part: it's forced once (the boss dedups
/// in-flight drives), and both resolve from the one registry slot.
#[test]
fn two_strands_demanding_one_part_both_resolve() {
    let s = Scheduler::new(&[4]);
    let (handle, producer) = s.block_on(remote::channel());
    let hid = s.register_handle(handle);
    let a = expect_bytes(&s, hid, "shared", b"X");
    let b = expect_bytes(&s, hid, "shared", b"X");
    s.demand(a);
    s.demand(b);
    let writer = std::thread::spawn(move || {
        producer.write_path("shared", b"X");
        producer
    });
    s.run();
    let _ = writer.join();
    assert_eq!(s.outcome(a), Some(Outcome::Clean));
    assert_eq!(s.outcome(b), Some(Outcome::Clean));
}
