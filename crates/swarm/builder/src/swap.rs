//! SWAP settlement wiring for the client launch path.
//!
//! Ties three pieces together behind the `swap` feature: a [`SwapProvider`]
//! registered with the accounting builder so [`BandwidthMode`] selection drives
//! cheque issuance, the [`SwapService`] actor that owns the cheque-exchange state
//! machine, and the wire plumbing that routes swap events from the node into the
//! service and the service's `SendCheque` commands back to the node.
//!
//! The provider and the service share one accounting instance: the provider
//! delegates outbound settlement to the service through a [`SwapHandle`], and the
//! service credits received cheques against the same balances. Because the
//! provider must be embedded in the accounting before it is built, the wiring is
//! split: [`SwapWiring::prepare`] builds the handle and provider up front, then
//! [`SwapWiring::spawn`] constructs and spawns the service once the accounting
//! instance and the node command channel exist.
//!
//! Cheque exchange is chain-free. With the `chain` feature also enabled, a
//! cashout client built over the shared provider redeems received cheques on
//! chain.

use std::sync::Arc;

use alloy_chains::NamedChain;
use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use tokio::sync::mpsc;
use tracing::{info, warn};
use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmIdentity, SwarmSpec};
use vertex_swarm_bandwidth_swap::service::SwapCommand;
use vertex_swarm_bandwidth_swap::{SwapEvent, SwapHandle, SwapProvider, SwapService};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::SwapConfig;
use vertex_swarm_node::{ClientCommand, ClientHandle};
use vertex_swarm_spec::Spec;

#[cfg(feature = "chain")]
use crate::chain::SharedChainProvider;

/// Resolved swap settlement parameters and the channels that connect the
/// provider, the service, and the node.
///
/// Produced by [`SwapWiring::prepare`] before the accounting is built; consumed
/// by [`SwapWiring::spawn`] after the node command channel exists.
pub(crate) struct SwapWiring {
    command_rx: mpsc::UnboundedReceiver<SwapCommand>,
    swap_event_tx: mpsc::UnboundedSender<SwapEvent>,
    swap_event_rx: mpsc::UnboundedReceiver<SwapEvent>,
    signer: Arc<PrivateKeySigner>,
    our_rate: U256,
    chequebook: Address,
    beneficiary: Address,
    chain: NamedChain,
}

impl SwapWiring {
    /// Build the swap handle and provider when SWAP settlement is enabled.
    ///
    /// Returns `None` (and leaves accounting swap-free) when the bandwidth mode
    /// does not enable SWAP, or when SWAP is requested but the required chequebook
    /// address and settlement chain cannot be resolved. The returned provider is
    /// registered with the accounting builder; the returned wiring is later handed
    /// to [`SwapWiring::spawn`].
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
            if swap_config.enable {
                warn!(
                    ?mode,
                    "--swap set but bandwidth mode does not enable SWAP; settlement not wired"
                );
            }
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

        // The beneficiary defaults to the node Ethereum address: the only payout
        // address a cheque sent to us may name.
        let beneficiary = swap_config
            .beneficiary
            .unwrap_or_else(|| identity.ethereum_address());

        let (swap_event_tx, swap_event_rx) = mpsc::unbounded_channel();
        // The handle backs the provider; its command channel is drained by the
        // service spawned in `spawn`. The handle is created here so the provider
        // can be embedded in the accounting before the accounting is built.
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let handle = SwapHandle::new(command_tx);
        let provider = SwapProvider::with_handle(config.clone(), handle);

        info!(%chequebook, %beneficiary, %chain, "SWAP settlement enabled");

        let wiring = Self {
            command_rx,
            swap_event_tx,
            swap_event_rx,
            signer: identity.signer(),
            our_rate: U256::from(config.refresh_rate()),
            chequebook,
            beneficiary,
            chain,
        };

        Some((provider, wiring))
    }

    /// The sender the node behaviour routes swap wire events into.
    pub(crate) fn swap_event_sender(&self) -> mpsc::UnboundedSender<SwapEvent> {
        self.swap_event_tx.clone()
    }

    /// Construct, spawn, and wire the swap service.
    ///
    /// The service records cheque-driven balance changes against `accounting`
    /// (the same instance the provider settles through), drains the provider's
    /// command channel, and consumes routed swap wire events. Its `SendCheque`
    /// commands are forwarded to the node through `client_handle`. With the
    /// `chain` feature and a connected provider, received cheques are also cashed
    /// on chain, paying out to our beneficiary.
    pub(crate) fn spawn<A>(
        self,
        ctx: &dyn InfrastructureContext,
        accounting: Arc<A>,
        client_handle: ClientHandle,
        #[cfg(feature = "chain")] chain_provider: Option<&SharedChainProvider>,
    ) where
        A: SwarmBandwidthAccounting + 'static,
    {
        // The service speaks unbounded `ClientCommand`; the node command channel
        // is bounded and reached through `ClientHandle::send_command`. Bridge the
        // two with a forwarding task so the service never blocks on a full queue.
        let (client_command_tx, client_command_rx) = mpsc::unbounded_channel();
        spawn_command_bridge(ctx, client_command_rx, client_handle);

        let service = SwapService::new(
            self.command_rx,
            self.swap_event_rx,
            client_command_tx,
            accounting,
            self.our_rate,
            self.signer,
            self.chequebook,
            self.beneficiary,
            self.chain,
        );

        #[cfg(feature = "chain")]
        let service = attach_cashout(service, chain_provider, self.beneficiary);

        ctx.executor().spawn_service("swarm.swap_service", service);
    }
}

/// Forward the swap service's `ClientCommand`s to the node command channel.
///
/// The service emits commands on an unbounded channel; this task drains it and
/// hands each command to the node through `ClientHandle::send_command`, which is
/// non-blocking. The task ends when the service drops its sender or on shutdown.
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

/// Attach an on-chain cashout client to the swap service when a chain provider is
/// present, so received cheques are redeemed paying out to our beneficiary.
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
