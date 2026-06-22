//! The producer-half connection: a consumer forces parts of a remote tree over a
//! `Producing` service reached through a **virtual connection** — the mechanism the
//! exec output crossing uses (an executor serves its outputs; the orchestrator opens
//! a `Producing` vconn over the same link and `force_remote` pulls `force(PartKey)`).
//!
//! This is request/response demand-pull done the right way (a virtual connection),
//! *not* faked over a channel pair. It reuses the built `Producing`/`force_remote`
//! machinery verbatim — the only new seam is obtaining the `ProducingClient` from
//! `session.open` instead of `remote::channel`.

use vox::ConnectionSettings;
use vx_runtime::remote::{self, Part, PartKey, Producer, ProducingClient, ProducingDispatcher};
use vx_runtime::strand::{Outcome, ResourceId, Scheduler, force_remote};

/// Stand up an "executor" serving its outputs via the `Producing` service over a
/// shared link, and return a root client (to open virtual connections from) plus the
/// `Producer` control (to make outputs available).
async fn executor() -> (ProducingClient, Producer) {
    let (service, producer) = remote::producer();
    let (client_link, server_link) = vox::memory_link_pair(64);
    tokio::spawn(async move {
        let acceptor =
            vox::lane_acceptor_fn(move |request: &vox::LaneRequest, conn: vox::PendingLane| {
                match request.service() {
                    // Serves both the root connection and inbound virtual connections.
                    "Producing" => {
                        conn.handle_with(ProducingDispatcher::new(service.clone()));
                        Ok(())
                    }
                    _ => Err(vox::LaneRejection::new(
                        vox::LaneRejectReason::UnknownService,
                    )),
                }
            });
        if let Ok(connection) = vox::acceptor_on(server_link)
            .on_lane(acceptor)
            .establish_connection()
            .await
        {
            connection.closed().await;
        }
    });
    let root = vox::initiator_on(client_link)
        .establish::<ProducingClient>()
        .await
        .expect("establish root Producing client");
    (root, producer)
}

/// Open a `Producing` virtual connection over the orchestrator's session, wrap it in
/// a `RemoteHandle`, register it, and force a path from a strand — the bytes the
/// executor produced come back over the virtual connection.
#[test]
fn force_remote_pulls_a_tree_path_over_a_virtual_connection() {
    let s = Scheduler::new(&[4]);
    let (root, producer) = s.block_on(executor());
    producer.write_path("out", b"VCONN");

    // The orchestrator opens a `Producing` virtual connection on the observe session.
    let vconn: ProducingClient = s.block_on(async {
        root.connection
            .as_ref()
            .expect("client exposes a connection handle")
            .open_lane_with_settings::<ProducingClient>(ConnectionSettings {
                parity: vox::Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            })
            .await
            .expect("open Producing virtual connection")
    });
    let hid = s.register_handle(remote::RemoteHandle::new(vconn));

    let strand = s.spawn(ResourceId(0), move || {
        match &*force_remote(hid, PartKey::TreePath("out".to_string())) {
            Part::Bytes(b) if b == b"VCONN" => Outcome::Clean,
            Part::Bytes(_) => Outcome::Poisoned("wrong bytes".into()),
            Part::Absent(why) => Outcome::Poisoned(format!("absent: {why}")),
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(s.outcome(strand), Some(Outcome::Clean));
    drop(root); // hold the root session + its vconn alive until the run completes
    drop(producer);
}
