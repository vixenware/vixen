//! Remote handles — values produced on one executor and consumed on another that
//! resolve **in parts, over time**. This is the build system's spine
//! (the demand-driven runtime, wiki), not a bolt-on: every cross-executor value
//! crosses as a *handle*, the consumer **pulls** parts on demand, forcing an
//! unproduced part **blocks** until it lands, and a part the producer never makes
//! **poisons**. Nothing is shipped eagerly or whole.
//!
//! The producer↔consumer exchange runs over **vox** (`transport-local`, an
//! in-process memory link) — the real RPC wire the rest of the system speaks, not
//! ad-hoc channels. Locality is incidental (§2): the same [`Producing`] service and
//! [`ProducingClient`] surface work over TCP/WebSocket to an actual executor.
//!
//! The canonical test is **Cargo pipelining**: a producing tree serves
//! `libA.rmeta` to a consumer while `libA.rlib` is still being produced — exactly
//! what an atomic / resolve-at-completion output tree cannot do.
//!
//! Wired into the scheduler: `strand::force_remote` registers a handle's part,
//! parks the strand if it isn't resolved, and the boss drives the `force()` here on
//! its runtime — a strand suspends on a remote part and resumes when it lands (see
//! `strand.rs` and `tests/remote_parts_on_strands.rs`). What's *not* yet wired is
//! eval's own `force` calling `force_remote`: eval has no remote values until `exec`
//! produces a handle, so that join comes with `exec`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use facet::Facet;
use tokio::sync::Notify;

/// A part of a produced value, addressed by the consumer. Resolution granularity
/// is per-kind (§3): a tree by **path**; later, a stream by chunk, an observer by
/// field. Scalars have no parts (they cross resolved).
#[derive(Facet, Debug, Clone, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PartKey {
    /// A path within a producing tree — `tree[path]`.
    TreePath(String),
    /// An item of a producing stream, by 0-based position. A consumer walks
    /// `0, 1, 2, …` until [`Part::Absent`] marks the end — the streaming dual of
    /// `TreePath` (a `run.stream.stdout` chunk; an observer's nth emitted value).
    StreamItem(u64),
}

/// The producer's answer for a demanded part.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum Part {
    /// The part's bytes (a tree path's file content).
    Bytes(Vec<u8>),
    /// The producer ended (or died) without ever making this part — a poison,
    /// carrying the reason.
    Absent(String),
}

/// The cross-executor RPC surface: the consumer demands a part; the producer holds
/// the call open until it yields that part (or finishes without it). One blocking
/// request-response per part — the demand-pull model, over the wire.
#[vox::service]
pub trait Producing {
    /// Force one part. Blocks (server-side) until the producer writes it, then
    /// returns its bytes; returns [`Part::Absent`] if the producer finishes first.
    async fn force(&self, part: PartKey) -> Part;
}

/// The producer's shared state: written parts + a `Notify` that wakes the blocked
/// `force` calls whenever a part lands or the producer finishes.
struct Shared {
    state: Mutex<ProducerState>,
    notify: Notify,
}

#[derive(Default)]
struct ProducerState {
    written: HashMap<PartKey, Vec<u8>>,
    finished: bool,
}

/// The **producer side** of the [`Producing`] service — a running exec's output.
/// Each `force` call awaits the part it wants; the [`Producer`] control writes
/// parts (waking awaiters) over time.
#[derive(Clone)]
pub struct ProducerService(Arc<Shared>);

impl Producing for ProducerService {
    async fn force(&self, part: PartKey) -> Part {
        loop {
            // Register interest *before* checking, so a write between the check and
            // the await can't be missed (tokio's documented `Notified::enable`).
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            {
                let st = self.0.state.lock().unwrap();
                if let Some(bytes) = st.written.get(&part) {
                    return Part::Bytes(bytes.clone());
                }
                if st.finished {
                    return Part::Absent(format!("producer finished without {part:?}"));
                }
                notified.as_mut().enable();
            } // lock released before awaiting — the future stays `Send`
            notified.await;
        }
    }
}

/// The **producer control**: make parts available over time. Shares state with the
/// served [`ProducerService`]; writing a part wakes any consumer blocked on it.
#[derive(Clone)]
pub struct Producer(Arc<Shared>);

impl Producer {
    /// Make one part available, waking any consumer blocked on it.
    fn write(&self, key: PartKey, bytes: &[u8]) {
        self.0
            .state
            .lock()
            .unwrap()
            .written
            .insert(key, bytes.to_vec());
        self.0.notify.notify_waiters();
    }

    /// A path has been produced — its bytes are now available to consumers.
    pub fn write_path(&self, path: &str, bytes: &[u8]) {
        self.write(PartKey::TreePath(path.to_string()), bytes);
    }

    /// A stream item has been produced — item `index` (0-based) is now available.
    pub fn write_item(&self, index: u64, bytes: &[u8]) {
        self.write(PartKey::StreamItem(index), bytes);
    }

    /// The producer is done — no more parts. Any consumer blocked on an unwritten
    /// part now gets [`Part::Absent`].
    pub fn finish(&self) {
        self.0.state.lock().unwrap().finished = true;
        self.0.notify.notify_waiters();
    }
}

/// The **consumer side** — a handle to a producer on another peer, backed by a vox
/// client. It can only demand parts (it cannot see the producer's state). Cloneable
/// so several strands demand parts concurrently.
#[derive(Clone)]
pub struct RemoteHandle {
    client: Arc<ProducingClient>,
}

impl RemoteHandle {
    /// Force one part: a vox `force` call that blocks until the producer yields it
    /// or declares it [`Part::Absent`]. `Err` only if the connection is gone.
    pub async fn force(&self, key: PartKey) -> Result<Part, &'static str> {
        self.client
            .force(key)
            .await
            .map_err(|_| "producer connection closed")
    }

    /// Force a tree path (`tree[path]`).
    pub async fn force_path(&self, path: &str) -> Result<Part, &'static str> {
        self.force(PartKey::TreePath(path.to_string())).await
    }

    /// Force a stream item (`stream[index]`) — the consumer walks `0, 1, 2, …`.
    pub async fn force_item(&self, index: u64) -> Result<Part, &'static str> {
        self.force(PartKey::StreamItem(index)).await
    }
}

/// Stand up a producer and a consumer connected by an in-process vox memory link
/// (the same `establish` dance the daemon uses for in-process services). Returns
/// the consumer-side [`RemoteHandle`] and the producer-side [`Producer`]; they
/// share no memory — only the vox connection.
pub async fn channel() -> (RemoteHandle, Producer) {
    let shared = Arc::new(Shared {
        state: Mutex::new(ProducerState::default()),
        notify: Notify::new(),
    });
    let service = ProducerService(Arc::clone(&shared));

    let (client_link, server_link) = vox::memory_link_pair(64);
    tokio::spawn(async move {
        let acceptor = vox::acceptor_fn(
            move |request: &vox::ConnectionRequest, connection: vox::PendingConnection| {
                match request.service() {
                    "Producing" => {
                        connection.handle_with(ProducingDispatcher::new(service.clone()));
                        Ok(())
                    }
                    _ => Err(vec![]),
                }
            },
        );
        if let Ok(client) = vox::acceptor_on(server_link)
            .on_connection(acceptor)
            .establish::<vox::NoopClient>()
            .await
        {
            client.caller.closed().await;
        }
    });

    let client = vox::initiator_on(client_link, vox::TransportMode::Bare)
        .establish::<ProducingClient>()
        .await
        .expect("establish in-process Producing client");
    (
        RemoteHandle {
            client: Arc::new(client),
        },
        Producer(shared),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical cross-executor test: Cargo pipelining, over vox. A producing
    /// tree serves `libA.rmeta` (end-of-type-check) to a consumer while `libA.rlib`
    /// (codegen) is still being produced — and `rlib` blocks until it lands. No
    /// atomic / resolve-at-completion output tree can pass this.
    #[tokio::test]
    async fn cargo_pipelining_serves_rmeta_before_rlib() {
        let (handle, producer) = channel().await;

        // `rustc` on executor A emits rmeta at end-of-type-check; rlib comes later.
        producer.write_path("libA.rmeta", b"META");

        // Consumer on executor B forces rmeta — resolves NOW, without rlib existing.
        assert_eq!(
            handle.force_path("libA.rmeta").await.unwrap(),
            Part::Bytes(b"META".to_vec()),
            "rmeta resolves while rlib is still being produced"
        );

        // Forcing rlib blocks (the vox call holds open server-side): spawn it,
        // confirm it can't finish until the write below, then let it land.
        let h2 = handle.clone();
        let rlib = tokio::spawn(async move { h2.force_path("libA.rlib").await });
        tokio::task::yield_now().await;
        assert!(
            !rlib.is_finished(),
            "rlib blocks while codegen is in flight"
        );

        producer.write_path("libA.rlib", b"LIB");
        assert_eq!(
            rlib.await.unwrap().unwrap(),
            Part::Bytes(b"LIB".to_vec()),
            "rlib resolves once produced"
        );
    }

    /// Forcing a part the producer never makes poisons once it finishes.
    #[tokio::test]
    async fn unproduced_part_poisons_when_the_producer_finishes() {
        let (handle, producer) = channel().await;
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
        let (handle, producer) = channel().await;

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

    /// The streaming dual: a producing stream whose items resolve over time and out
    /// of order, a consumer that walks `0, 1, 2, …`, and the first item the producer
    /// never makes poisoning at `finish` — the end-of-stream terminator. This is the
    /// `run.stream.stdout` invariant the same handle serves for chunks.
    #[tokio::test]
    async fn stream_items_resolve_over_time_then_end() {
        let (handle, producer) = channel().await;

        // A consumer walking the stream blocks on item 0 until it lands.
        let h0 = handle.clone();
        let item0 = tokio::spawn(async move { h0.force_item(0).await });
        tokio::task::yield_now().await;
        assert!(!item0.is_finished(), "item 0 blocks until produced");

        // Items produced out of order; each blocked force resolves to its own item.
        producer.write_item(1, b"one");
        producer.write_item(0, b"zero");
        assert_eq!(item0.await.unwrap().unwrap(), Part::Bytes(b"zero".to_vec()));
        assert_eq!(
            handle.force_item(1).await.unwrap(),
            Part::Bytes(b"one".to_vec())
        );

        // The stream ends after item 1: forcing item 2 blocks, then poisons at finish.
        let h2 = handle.clone();
        let item2 = tokio::spawn(async move { h2.force_item(2).await });
        tokio::task::yield_now().await;
        assert!(
            !item2.is_finished(),
            "the tail blocks until the stream ends"
        );
        producer.finish();
        assert!(
            matches!(item2.await.unwrap().unwrap(), Part::Absent(_)),
            "the item past the end poisons — the stream terminator"
        );
    }
}
