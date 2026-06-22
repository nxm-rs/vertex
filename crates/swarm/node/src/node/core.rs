//! Shared client-core assembly surface for the native builder and the embedded
//! launcher.
//!
//! Both client entry points wire the same accounting/selector/throttle middle.
//! This module owns the pieces that middle needs to be reachable from the wasm
//! launcher: the concrete shared accounting alias, the pseudosettle service
//! wiring, and the command bridge that drains a settlement service onto the node
//! command channel. Spawning takes a bare [`TaskExecutor`] so both the native
//! context and the browser launcher drive it.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;
use vertex_swarm_accounting::{
    Accounting, AccountingBuilder, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
};
use vertex_swarm_accounting_pseudosettle::{
    PseudosettleCommand, PseudosettleEvent, PseudosettleHandle, PseudosettleProvider,
    PseudosettleService,
};
use vertex_swarm_api::{
    Au, PeerReporter, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmClientAccounting,
    SwarmSettlementProvider,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::TaskExecutor;

use crate::{
    AccountingSettlement, ClientCommand, ClientHandle, ClientService, PeerSelector, SelfThrottle,
};

/// The concrete shared accounting both client-backed node types build: the
/// default bandwidth accounting wrapped with the config pricer, pinned to the
/// node identity. One instance is shared across the selector, throttle,
/// forwarder, client service, and settlement services.
pub type SharedAccounting = Arc<
    ClientAccounting<Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>, FixedPricer<Spec>>,
>;

/// The shared client middle both client-backed entry points assemble: the
/// accounting, the candidate selector, the outbound self-throttle, the throttled
/// client handle, and the accounting-attached client service.
///
/// Provider-free by design: the chunk provider lives in the native builder
/// (which depends up on this crate) and is also the RPC providers payload, so
/// each entry point builds its own from `throttled_handle` and `selector` after
/// calling [`assemble_client_core`]. `enable_forwarding` is likewise the entry
/// point's call: it borrows the node mutably before the node moves into the run
/// loop, so the borrow and the move stay in one scope.
pub struct ClientCore {
    /// The one accounting instance shared across selector, throttle, forwarder,
    /// service, and settlement.
    pub accounting: SharedAccounting,
    /// Retrieval and pushsync candidate selection over the shared accounting.
    pub selector: Arc<PeerSelector>,
    /// Outbound self-throttle shared by both protocols and the service.
    pub throttle: Arc<SelfThrottle>,
    /// The client handle paced by `throttle`; the provider dispatches through it.
    pub throttled_handle: ClientHandle,
    /// The node's topology handle, threaded through unchanged.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The client service with accounting, throttle, and reporter attached.
    pub client_service: ClientService,
    /// The unthrottled client handle settlement services forward commands to.
    pub client_handle: ClientHandle,
}

/// Inputs to [`assemble_client_core`].
///
/// Carries the prepared pseudosettle provider plus any native-only providers
/// (swap) in `extra_settlement`; pseudosettle is registered first so soft
/// accounting forgives total debt before swap settles originated debt.
pub struct ClientCoreCtx {
    /// Network spec for the config pricer.
    pub spec: Arc<Spec>,
    /// Node identity the accounting and overlay are pinned to.
    pub identity: Arc<Identity>,
    /// Bandwidth config; the accounting builder consumes a clone and the
    /// throttle derives its sizing from a borrow.
    pub bandwidth: DefaultBandwidthConfig,
    /// The node's topology handle.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The client service to attach accounting, throttle, and reporter to.
    pub client_service: ClientService,
    /// The unthrottled client handle.
    pub client_handle: ClientHandle,
    /// Soft-accounting settlement, registered first.
    pub pseudosettle_provider: PseudosettleProvider<DefaultBandwidthConfig>,
    /// Native-only settlement providers (swap) registered after pseudosettle.
    pub extra_settlement: Vec<Box<dyn SwarmSettlementProvider>>,
    /// The peer-scoring authority accounting and the service report through.
    pub reporter: Arc<dyn PeerReporter>,
}

/// Assemble the shared client middle: build the accounting with its settlement
/// providers, the selector and throttle over it, the throttled handle, and the
/// accounting-attached service.
///
/// Does not build a chunk provider and does not call `enable_forwarding`; both
/// stay with the caller. Returns the shared accounting `Arc` so the caller can
/// thread it into `enable_forwarding` and keep it alive for the run loop.
pub fn assemble_client_core(ctx: ClientCoreCtx) -> ClientCore {
    let ClientCoreCtx {
        spec,
        identity,
        bandwidth,
        topology,
        client_service,
        client_handle,
        pseudosettle_provider,
        extra_settlement,
        reporter,
    } = ctx;

    // Pseudosettle is registered first so soft accounting forgives total debt
    // before swap settles originated debt; the order matches `settle_all`.
    let accounting = AccountingBuilder::new(bandwidth.clone())
        .with_pricer_from_config(spec)
        .with_reporter(Arc::clone(&reporter))
        .with_settlement(pseudosettle_provider)
        .with_settlements(extra_settlement)
        .build(&identity);
    // One accounting instance is shared by the selector, throttle, forwarder,
    // service, and settlement services.
    let accounting: SharedAccounting = Arc::new(accounting);

    let selector = Arc::new(PeerSelector::new(
        Arc::new(topology.clone()),
        accounting.bandwidth().clone(),
        Arc::new(accounting.pricing().clone()),
        Arc::new(AccountingSettlement::new(accounting.bandwidth().clone())),
    ));

    // Outbound self-throttle: pace our retrieval and pushsync requests under each
    // peer's pseudosettle allowance so a burst never crosses the settlement
    // trigger.
    let throttle = Arc::new(SelfThrottle::new(&accounting, &bandwidth));
    let throttled_handle = client_handle.clone().with_throttle(Arc::clone(&throttle));

    // The service reports through the same peer-manager authority accounting
    // uses, shares the handle's throttle so a disconnect clears that peer's
    // bucket, and debits the serving peer for our own-request deliveries through
    // the same shared accounting the selector, throttle, and forwarder use.
    let client_service = client_service
        .with_reporter(reporter)
        .with_throttle(Arc::clone(&throttle))
        .with_accounting(
            Arc::new(accounting.pricing().clone()),
            accounting.bandwidth().clone(),
        );

    ClientCore {
        accounting,
        selector,
        throttle,
        throttled_handle,
        topology,
        client_service,
        client_handle,
    }
}

/// Channels connecting the pseudosettle provider, service, and node.
///
/// Produced by [`PseudosettleWiring::prepare`] before the accounting is built;
/// consumed by [`PseudosettleWiring::spawn`] after the node command channel
/// exists. Wasm-clean: tokio sync channels and an `Au` refresh rate only.
pub struct PseudosettleWiring {
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
    pub fn prepare<C>(config: &C) -> (PseudosettleProvider<C>, Self)
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
    pub fn event_sender(&self) -> mpsc::UnboundedSender<PseudosettleEvent> {
        self.event_tx.clone()
    }

    /// Construct, spawn, and wire the pseudosettle service.
    ///
    /// The service applies time-based refresh against `accounting` (the same
    /// instance the provider settles through), drains the provider's command
    /// channel, and consumes routed pseudosettle wire events. Settlement
    /// violations are reported through `reporter`. Its outbound `SendPseudosettle`
    /// commands are forwarded to the node through `client_handle`.
    pub fn spawn<A>(
        self,
        executor: &TaskExecutor,
        accounting: Arc<A>,
        client_handle: ClientHandle,
        reporter: Arc<dyn PeerReporter>,
    ) where
        A: SwarmBandwidthAccounting + 'static,
    {
        // The service speaks unbounded `ClientCommand`; bridge it to the bounded
        // node command channel so the service never blocks on a full queue.
        let (client_command_tx, client_command_rx) = mpsc::unbounded_channel();
        spawn_client_command_bridge(
            executor,
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

        executor.spawn_service("swarm.pseudosettle_service", service);
    }
}

/// Forward a settlement service's `ClientCommand`s to the node command channel.
///
/// A settlement service (pseudosettle or swap) emits commands on an unbounded
/// channel; this task drains it and hands each command to the node through the
/// non-blocking [`ClientHandle::send_command`], so the service never blocks on a
/// full queue. The task ends when the service drops its sender or on shutdown.
pub fn spawn_client_command_bridge(
    executor: &TaskExecutor,
    task_name: &'static str,
    mut command_rx: mpsc::UnboundedReceiver<ClientCommand>,
    client_handle: ClientHandle,
) {
    executor.spawn_with_graceful_shutdown_signal(task_name, move |shutdown| async move {
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    drop(guard);
                    break;
                }
                command = command_rx.recv() => {
                    let Some(command) = command else { break };
                    if let Err(e) = client_handle.send_command(command) {
                        warn!(error = %e, "Failed to forward settlement command to node");
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use vertex_swarm_accounting::AccountingBuilder;
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
