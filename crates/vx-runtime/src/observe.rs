//! The `observe` RPC — the orchestrator ships an observer bundle to an executor (a
//! fresh **mirror** runtime) over **vox**; the executor installs it, evaluates the
//! entry body, and returns the result. In-process uses a `transport-local` memory
//! link; a remote executor is a different transport, the *same* service. Vox owns
//! the serialization — this code never names a codec (the wire format is vox's to
//! choose and to change).

use vx_ir::Bundle;

use crate::eval::{KernelMode, Outcome, Value, eval_body};

/// The result of observing. Minimal for now — a scalar or a poison reason; the
/// general value-crossing (structured results, the producing handle for streams)
/// lands with the value model.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum Observed {
    /// The observer returned a `U64`.
    U64(u64),
    /// The observer poisoned, or returned a value we can't carry yet.
    Poisoned(String),
}

/// The runner-side service: ship a bundle, get the observer's result.
#[vox::service]
pub trait Observe {
    async fn observe(&self, bundle: Bundle) -> Observed;
}

/// The executor side — a fresh mirror per call: install the bundle, evaluate its
/// entry body, return the result. Never compiles, never allocates an id.
#[derive(Clone, Default)]
pub struct ObserveService;

impl Observe for ObserveService {
    async fn observe(&self, bundle: Bundle) -> Observed {
        let (program, env) = bundle.install();
        match eval_body(&program, &env, bundle.entry, vec![], KernelMode::Off) {
            Outcome::Clean(Value::U64(n)) => Observed::U64(n),
            Outcome::Clean(other) => Observed::Poisoned(format!("non-scalar result: {other:?}")),
            Outcome::Poisoned { reason, .. } => Observed::Poisoned(reason),
        }
    }
}

/// Stand up an [`ObserveService`] over an in-process vox memory link and return a
/// client. The bundle crosses as vox bytes — colocated today, a remote executor
/// tomorrow, the same service either way.
pub async fn local_observer() -> ObserveClient {
    let (client_link, server_link) = vox::memory_link_pair(64);
    tokio::spawn(async move {
        let acceptor = vox::acceptor_fn(
            |request: &vox::ConnectionRequest, connection: vox::PendingConnection| match request
                .service()
            {
                "Observe" => {
                    connection.handle_with(ObserveDispatcher::new(ObserveService));
                    Ok(())
                }
                _ => Err(vec![]),
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
    vox::initiator_on(client_link, vox::TransportMode::Bare)
        .establish::<ObserveClient>()
        .await
        .expect("establish in-process Observe client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vx_ir::compile::compile_source;
    use vx_ir::item::ItemKind;
    use vx_ir::reachable;

    /// The spine's first real wire hop: bundle an observer body, ship it to a fresh
    /// mirror **over vox** (transport-local), and get its evaluated result back. The
    /// bundle is serialized by vox — nothing here knows or names the codec.
    #[tokio::test]
    async fn observe_ships_a_bundle_over_vox_and_evaluates_it() {
        let fe =
            compile_source("fn answer() -> U64 ( 21 + 21 )", "answer").expect("front-end compiles");
        let entry = fe
            .program
            .arena
            .items
            .iter()
            .find_map(|it| match &it.kind {
                ItemKind::Function(f) if f.name == "answer" => Some(f.body),
                _ => None,
            })
            .expect("answer fn");
        let bundle = reachable(&fe.program.arena, &fe.env, entry);

        let client = local_observer().await;
        let observed = client.observe(bundle).await.expect("observe over vox");
        assert_eq!(observed, Observed::U64(42));
    }
}
