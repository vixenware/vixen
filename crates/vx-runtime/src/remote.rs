//! Remote handles — values produced on one executor and consumed on another that
//! resolve **in parts, over time**. This is the build system's spine
//! (`runtime-semantics.md` §3, §4), not a bolt-on: every cross-executor value
//! crosses as a *handle*, the consumer **pulls** parts on demand, forcing an
//! unproduced part **blocks** until it lands, and a part the producer never makes
//! **poisons**. Nothing is shipped eagerly or whole.
//!
//! This module is the model plus a two-peer harness. The canonical test is **Cargo
//! pipelining**: a producing tree serves `libA.rmeta` to a consumer on another peer
//! *while `libA.rlib` is still being produced* — exactly what an atomic / resolve-
//! at-completion output tree cannot do, and the workload the whole system exists to
//! serve. The transport here is an in-process async duplex (two tasks + channels);
//! it is faithful to the *semantics* (locality is incidental — §2), and the real
//! wire (vox) slots in behind the same [`RemoteHandle`] surface later.
//!
//! Not yet wired into the evaluator — forcing a [`RemoteHandle`] part is the async
//! analogue of `eval`'s `force`, and integrating it (a strand suspends on a remote
//! part) comes after the semantics are pinned here.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};

/// A part of a produced value, addressed by the consumer. Resolution granularity
/// is per-kind (§3): a tree by **path**; later, a stream by chunk, an observer by
/// field. Scalars have no parts (they cross resolved).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PartKey {
    /// A path within a producing tree — `tree[path]`.
    TreePath(String),
}

/// The producer's answer for a demanded part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// The part's bytes (a tree path's file content).
    Bytes(Vec<u8>),
    /// The producer ended (or died) without ever making this part — a poison,
    /// carrying the reason.
    Absent(String),
}

/// One demand crossing the connection: which part, and a one-shot channel for the
/// producer's answer.
struct Demand {
    key: PartKey,
    reply: oneshot::Sender<Part>,
}

/// What the producer is told to do (driven by the running `exec` / sandbox writes;
/// in the harness, by the test).
enum Control {
    /// A path has been written — its bytes are now available.
    Write { key: PartKey, bytes: Vec<u8> },
    /// The producer has finished; no further parts will appear.
    Finish,
}

/// The **consumer** side of a producing value — a handle to a producer on another
/// peer. It holds only a demand channel; it cannot see the producer's state and
/// must request every part. Cloneable, so several strands demand parts concurrently.
#[derive(Clone)]
pub struct RemoteHandle {
    demands: mpsc::UnboundedSender<Demand>,
}

impl RemoteHandle {
    /// Force one part: send the demand across the connection and await the answer.
    /// **Blocks** (suspends this strand) until the producer yields the part or
    /// declares it [`Part::Absent`]. `Err` only if the connection is gone.
    pub async fn force(&self, key: PartKey) -> Result<Part, &'static str> {
        let (reply, rx) = oneshot::channel();
        self.demands
            .send(Demand { key, reply })
            .map_err(|_| "producer connection closed")?;
        rx.await.map_err(|_| "producer dropped the demand")
    }

    /// Force a tree path (`tree[path]`).
    pub async fn force_path(&self, path: &str) -> Result<Part, &'static str> {
        self.force(PartKey::TreePath(path.to_string())).await
    }
}

/// The **producer** side — a running `exec`'s output being written over time. Each
/// [`write_path`](Self::write_path) makes a part available (fulfilling any blocked
/// demands for it); [`finish`](Self::finish) poisons every still-blocked demand.
/// This is the running sandbox's VFS, abstracted to the part level.
pub struct Producer {
    control: mpsc::UnboundedSender<Control>,
}

impl Producer {
    /// A path has been produced — its bytes are now available to consumers.
    pub fn write_path(&self, path: &str, bytes: &[u8]) {
        let _ = self.control.send(Control::Write {
            key: PartKey::TreePath(path.to_string()),
            bytes: bytes.to_vec(),
        });
    }

    /// The producer is done — no more parts. Any demand still blocked on an
    /// unwritten part now poisons ([`Part::Absent`]).
    pub fn finish(&self) {
        let _ = self.control.send(Control::Finish);
    }
}

/// Stand up a producer on its own task and return the two ends of the connection:
/// the consumer-side [`RemoteHandle`] and the producer-side [`Producer`]. They
/// share no memory — only the demand and control channels, modelling two
/// executors. The task ends when both ends are dropped.
pub fn channel() -> (RemoteHandle, Producer) {
    let (demand_tx, demand_rx) = mpsc::unbounded_channel::<Demand>();
    let (control_tx, control_rx) = mpsc::unbounded_channel::<Control>();
    tokio::spawn(serve(demand_rx, control_rx));
    (
        RemoteHandle { demands: demand_tx },
        Producer {
            control: control_tx,
        },
    )
}

/// The producer task: answer a demand for a written part at once; **park** a demand
/// for an unwritten part until a `Write` produces it; on `Finish`, poison every
/// still-parked demand. (The running sandbox VFS, at the part level.)
async fn serve(
    mut demands: mpsc::UnboundedReceiver<Demand>,
    mut control: mpsc::UnboundedReceiver<Control>,
) {
    let mut written: HashMap<PartKey, Vec<u8>> = HashMap::new();
    let mut parked: HashMap<PartKey, Vec<oneshot::Sender<Part>>> = HashMap::new();
    let mut finished = false;

    loop {
        tokio::select! {
            Some(d) = demands.recv() => {
                if let Some(bytes) = written.get(&d.key) {
                    let _ = d.reply.send(Part::Bytes(bytes.clone()));
                } else if finished {
                    let _ = d.reply.send(Part::Absent(format!("producer finished without {:?}", d.key)));
                } else {
                    parked.entry(d.key).or_default().push(d.reply);
                }
            }
            Some(c) = control.recv() => {
                match c {
                    Control::Write { key, bytes } => {
                        if let Some(waiters) = parked.remove(&key) {
                            for w in waiters {
                                let _ = w.send(Part::Bytes(bytes.clone()));
                            }
                        }
                        written.insert(key, bytes);
                    }
                    Control::Finish => {
                        finished = true;
                        for (key, waiters) in parked.drain() {
                            for w in waiters {
                                let _ = w.send(Part::Absent(format!("producer finished without {key:?}")));
                            }
                        }
                    }
                }
            }
            else => break, // both ends dropped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical cross-executor test: Cargo pipelining. A producing tree serves
    /// `libA.rmeta` (end-of-type-check) to a consumer on another peer while
    /// `libA.rlib` (codegen) is still being produced — and `rlib` blocks until it
    /// lands. No atomic / resolve-at-completion output tree can pass this.
    #[tokio::test]
    async fn cargo_pipelining_serves_rmeta_before_rlib() {
        let (handle, producer) = channel();

        // `rustc` on executor A emits rmeta at end-of-type-check; rlib comes later.
        producer.write_path("libA.rmeta", b"META");

        // Consumer on executor B forces rmeta — resolves NOW, without rlib existing.
        assert_eq!(
            handle.force_path("libA.rmeta").await.unwrap(),
            Part::Bytes(b"META".to_vec()),
            "rmeta resolves while rlib is still being produced"
        );

        // Forcing rlib blocks: spawn it, confirm it cannot finish (rlib unwritten —
        // its reply only fires on the write below, so this is deterministic, not a
        // timing race).
        let h2 = handle.clone();
        let rlib = tokio::spawn(async move { h2.force_path("libA.rlib").await });
        tokio::task::yield_now().await;
        assert!(
            !rlib.is_finished(),
            "rlib blocks while codegen is in flight"
        );

        // Codegen finishes: the blocked force resolves.
        producer.write_path("libA.rlib", b"LIB");
        assert_eq!(
            rlib.await.unwrap().unwrap(),
            Part::Bytes(b"LIB".to_vec()),
            "rlib resolves once produced"
        );
    }

    /// Forcing a part the producer never makes poisons once it finishes — the
    /// strand was blocked, then released as [`Part::Absent`].
    #[tokio::test]
    async fn unproduced_part_poisons_when_the_producer_finishes() {
        let (handle, producer) = channel();
        producer.write_path("out", b"ok");

        let h2 = handle.clone();
        let missing = tokio::spawn(async move { h2.force_path("never").await });
        tokio::task::yield_now().await;
        assert!(
            !missing.is_finished(),
            "the demand blocks until the producer ends"
        );

        producer.finish();
        assert!(
            matches!(missing.await.unwrap().unwrap(), Part::Absent(_)),
            "a never-produced part poisons at finish"
        );
        // A part written before finish still resolves cleanly.
        assert_eq!(
            handle.force_path("out").await.unwrap(),
            Part::Bytes(b"ok".to_vec())
        );
    }

    /// Independent, out-of-order, concurrent demands all resolve — parts resolve at
    /// their own time, not in lockstep.
    #[tokio::test]
    async fn parts_resolve_independently_and_out_of_order() {
        let (handle, producer) = channel();

        // Three consumers block on three different paths before any is written.
        let (a, b, c) = (handle.clone(), handle.clone(), handle.clone());
        let fa = tokio::spawn(async move { a.force_path("a").await });
        let fb = tokio::spawn(async move { b.force_path("b").await });
        let fc = tokio::spawn(async move { c.force_path("c").await });
        tokio::task::yield_now().await;

        // Produced out of demand order; each blocked force resolves to its own part.
        producer.write_path("b", b"B");
        producer.write_path("c", b"C");
        producer.write_path("a", b"A");

        assert_eq!(fa.await.unwrap().unwrap(), Part::Bytes(b"A".to_vec()));
        assert_eq!(fb.await.unwrap().unwrap(), Part::Bytes(b"B".to_vec()));
        assert_eq!(fc.await.unwrap().unwrap(), Part::Bytes(b"C".to_vec()));
    }
}
