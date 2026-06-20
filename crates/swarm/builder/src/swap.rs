//! SWAP settlement wiring behind the `swap` feature.
//!
//! The provider and the service share one accounting instance, so the wiring is
//! split: [`SwapWiring::prepare`] builds the handle and provider before the
//! accounting is built, then [`SwapWiring::spawn`] spawns the service once the
//! accounting instance and node command channel exist. With the `chain` feature
//! a cashout client redeems received cheques on chain.

use std::sync::Arc;

use alloy_chains::NamedChain;
use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use tokio::sync::mpsc;
use tracing::{info, warn};
use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{
    PeerReporter, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmIdentity, SwarmSpec,
};
use vertex_swarm_bandwidth_swap::service::SwapCommand;
use vertex_swarm_bandwidth_swap::{SwapEvent, SwapHandle, SwapProvider, SwapService};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::SwapConfig;
use vertex_swarm_node::{ClientCommand, ClientHandle};
use vertex_swarm_spec::Spec;

#[cfg(feature = "chain")]
use crate::chain::SharedChainProvider;

/// Resolved swap parameters and the channels connecting provider, service, and node.
pub(crate) struct SwapWiring {
    command_rx: mpsc::UnboundedReceiver<SwapCommand>,
    swap_event_tx: mpsc::UnboundedSender<SwapEvent>,
    swap_event_rx: mpsc::UnboundedReceiver<SwapEvent>,
    signer: Arc<PrivateKeySigner>,
    chequebook: Address,
    beneficiary: Address,
    chain: NamedChain,
}

impl SwapWiring {
    /// Build the swap handle and provider, or `None` when the mode does not enable
    /// SWAP or the chequebook and settlement chain cannot be resolved.
    pub(crate) fn prepare<C>(
        spec: &Arc<Spec>,
        identity: &Arc<Identity>,
        config: &C,
        swap_config: &SwapConfig,
    ) -> Option<(SwapProvider<C>, Self)>
    where
        C: SwarmAccountingConfig + Clone + 'static,
    {
        let mode = config.mode();
        if !mode.swap_enabled() {
            return None;
        }

        let Some(chequebook) = swap_config.chequebook else {
            warn!(
                "SWAP enabled but no --swap.chequebook configured; settlement not wired (chequebook deploy not yet supported)"
            );
            return None;
        };
        if swap_config.deploy {
            warn!("--swap.deploy is not yet supported; using the configured chequebook address");
        }

        let Some(chain) = spec.chain().named() else {
            warn!(
                "SWAP enabled but the network has no named settlement chain; settlement not wired"
            );
            return None;
        };

        let beneficiary = swap_config
            .beneficiary
            .unwrap_or_else(|| identity.ethereum_address());

        let (swap_event_tx, swap_event_rx) = mpsc::unbounded_channel();
        // Handle created here so the provider can be embedded in accounting before it is built.
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let handle = SwapHandle::new(command_tx);
        let provider = SwapProvider::with_handle(config.clone(), handle);

        info!(%chequebook, %beneficiary, %chain, "SWAP settlement enabled");

        let wiring = Self {
            command_rx,
            swap_event_tx,
            swap_event_rx,
            signer: identity.signer(),
            chequebook,
            beneficiary,
            chain,
        };

        Some((provider, wiring))
    }

    /// Sender the node behaviour routes swap wire events into.
    pub(crate) fn swap_event_sender(&self) -> mpsc::UnboundedSender<SwapEvent> {
        self.swap_event_tx.clone()
    }

    /// Construct, spawn, and wire the swap service against the shared `accounting`.
    pub(crate) fn spawn<A>(
        self,
        ctx: &dyn InfrastructureContext,
        accounting: Arc<A>,
        client_handle: ClientHandle,
        reporter: Arc<dyn PeerReporter>,
        #[cfg(feature = "chain")] chain_provider: Option<&SharedChainProvider>,
    ) where
        A: SwarmBandwidthAccounting + 'static,
    {
        // Bridge the service's unbounded commands to the bounded node channel so it never blocks.
        let (client_command_tx, client_command_rx) = mpsc::unbounded_channel();
        spawn_command_bridge(ctx, client_command_rx, client_handle);

        let service = SwapService::new(
            self.command_rx,
            self.swap_event_rx,
            client_command_tx,
            accounting,
            self.signer,
            self.chequebook,
            self.beneficiary,
            self.chain,
        )
        .with_reporter(reporter);

        #[cfg(feature = "chain")]
        let service = attach_cashout(service, chain_provider, self.beneficiary);

        ctx.executor().spawn_service("swarm.swap_service", service);
    }
}

/// Forward the swap service's `ClientCommand`s to the node command channel.
fn spawn_command_bridge(
    ctx: &dyn InfrastructureContext,
    mut client_command_rx: mpsc::UnboundedReceiver<ClientCommand>,
    client_handle: ClientHandle,
) {
    ctx.executor().spawn_with_graceful_shutdown_signal(
        "swarm.swap_command_bridge",
        move |shutdown| async move {
            let mut shutdown = std::pin::pin!(shutdown);
            loop {
                tokio::select! {
                    guard = &mut shutdown => {
                        drop(guard);
                        break;
                    }
                    command = client_command_rx.recv() => {
                        let Some(command) = command else { break };
                        if let Err(e) = client_handle.send_command(command) {
                            warn!(error = %e, "Failed to forward swap command to node");
                        }
                    }
                }
            }
        },
    );
}

/// Attach an on-chain cashout client when a chain provider is present.
#[cfg(feature = "chain")]
fn attach_cashout<A, S>(
    service: SwapService<A, S>,
    chain_provider: Option<&SharedChainProvider>,
    beneficiary: Address,
) -> SwapService<A, S>
where
    A: SwarmBandwidthAccounting + 'static,
    S: alloy_signer::SignerSync + Send + Sync + 'static,
{
    use vertex_swarm_bandwidth_swap::cashout::Cashout;

    let Some(provider) = chain_provider else {
        return service;
    };
    let cashout = Cashout::new(
        provider.provider().clone(),
        *provider.addresses(),
        beneficiary,
    );
    service.with_cashout(cashout)
}
