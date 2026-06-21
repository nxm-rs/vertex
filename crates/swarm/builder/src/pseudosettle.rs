//! Pseudosettle (soft accounting) settlement wiring for the client launch path.
//!
//! Pseudosettle is always on for client and storer nodes. It ties together a
//! [`PseudosettleProvider`] registered with the accounting builder, the
//! [`PseudosettleService`] actor that runs the time-based-refresh settlement,
//! and the wire plumbing that routes pseudosettle events from the node into the
//! service and the service's outbound `SendPseudosettle` commands back to the
//! node. Because the provider must be embedded in the accounting before it is
//! built, the wiring is split: [`PseudosettleWiring::prepare`] builds the handle
//! and provider up front, then [`PseudosettleWiring::spawn`] constructs and
//! spawns the service once the accounting instance and the node command channel
//! exist. Mirrors the SWAP wiring, minus the chain, signer, and chequebook.

use std::sync::Arc;

use tokio::sync::mpsc;
use vertex_node_api::InfrastructureContext;
use vertex_swarm_accounting_pseudosettle::{
    PseudosettleCommand, PseudosettleEvent, PseudosettleHandle, PseudosettleProvider,
    PseudosettleService,
};
use vertex_swarm_api::{Au, PeerReporter, SwarmAccountingConfig, SwarmBandwidthAccounting};
use vertex_swarm_node::ClientHandle;

/// Channels connecting the pseudosettle provider, service, and node.
///
/// Produced by [`PseudosettleWiring::prepare`] before the accounting is built;
/// consumed by [`PseudosettleWiring::spawn`] after the node command channel
/// exists.
pub(crate) struct PseudosettleWiring {
    command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
    event_tx: mpsc::UnboundedSender<PseudosettleEvent>,
    event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
    refresh_rate: Au,
}

impl PseudosettleWiring {
    /// Build the handle-backed provider and the wiring up front.
    ///
    /// The handle is created here so the provider can be embedded in the
    /// accounting before the accounting is built; its command channel is drained
    /// by the service spawned in [`Self::spawn`].
    pub(crate) fn prepare<C>(config: &C) -> (PseudosettleProvider<C>, Self)
    where
        C: SwarmAccountingConfig + Clone + 'static,
    {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let handle = PseudosettleHandle::new(command_tx);
        let provider = PseudosettleProvider::with_handle(config.clone(), handle);
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        (
            provider,
            Self {
                command_rx,
                event_tx,
                event_rx,
                refresh_rate: config.refresh_rate(),
            },
        )
    }

    /// The sender the node behaviour routes pseudosettle wire events into.
    pub(crate) fn event_sender(&self) -> mpsc::UnboundedSender<PseudosettleEvent> {
        self.event_tx.clone()
    }

    /// Construct, spawn, and wire the pseudosettle service.
    ///
    /// The service applies time-based refresh against `accounting` (the same
    /// instance the provider settles through), drains the provider's command
    /// channel, and consumes routed pseudosettle wire events. Settlement
    /// violations are reported through `reporter`. Its outbound `SendPseudosettle`
    /// commands are forwarded to the node through `client_handle`.
    pub(crate) fn spawn<A>(
        self,
        ctx: &dyn InfrastructureContext,
        accounting: Arc<A>,
        client_handle: ClientHandle,
        reporter: Arc<dyn PeerReporter>,
    ) where
        A: SwarmBandwidthAccounting + 'static,
    {
        // The service speaks unbounded `ClientCommand`; bridge it to the bounded
        // node command channel so the service never blocks on a full queue.
        let (client_command_tx, client_command_rx) = mpsc::unbounded_channel();
        crate::launch::spawn_client_command_bridge(
            ctx,
            "swarm.pseudosettle_command_bridge",
            client_command_rx,
            client_handle,
        );

        let service = PseudosettleService::new(
            self.command_rx,
            self.event_rx,
            client_command_tx,
            accounting,
            self.refresh_rate,
        )
        .with_reporter(reporter);

        ctx.executor()
            .spawn_service("swarm.pseudosettle_service", service);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use vertex_swarm_accounting::{AccountingBuilder, DefaultBandwidthConfig};
    use vertex_swarm_api::{SwarmClientAccounting, SwarmIdentity};
    use vertex_swarm_test_utils::test_identity_arc;

    #[test]
    fn default_client_accounting_wires_pseudosettle() {
        let identity = test_identity_arc();
        let config = DefaultBandwidthConfig::default();

        let (provider, wiring) = PseudosettleWiring::prepare(&config);
        assert_eq!(wiring.refresh_rate, config.refresh_rate());

        // Compose the accounting exactly as the launch tail does for a default
        // client: the pseudosettle provider is registered, so outbound settlement
        // has a mechanism instead of an empty provider list.
        let accounting = AccountingBuilder::new(config)
            .with_pricer_from_config(identity.spec().clone())
            .with_settlement(provider)
            .build(&identity);

        assert_eq!(
            accounting.bandwidth().provider_names(),
            vec!["pseudosettle"]
        );
    }
}
