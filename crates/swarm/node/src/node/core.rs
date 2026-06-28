//! Shared client-core assembly surface for the native builder and the embedded
//! launcher.
//!
//! Both client entry points wire the same accounting/selector middle.
//! This module owns the pieces that middle needs to be reachable from the wasm
//! launcher: the concrete shared accounting alias, the pseudosettle and (behind
//! the `swap` feature) swap service wiring, and the command bridge that drains a
//! settlement service onto the node command channel. Spawning takes a bare
//! [`TaskExecutor`] so both the native context and the browser launcher drive it.

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
    SwarmNodeType, SwarmSettlementProvider,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_peer_manager::{DEFAULT_TICK_INTERVAL, spawn_peer_manager_task};
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::TaskExecutor;

use crate::chunks::{ChunkVerifyConfig, NetworkChunkProvider, VerifyingChunkProvider};

#[cfg(feature = "swap")]
use alloy_chains::NamedChain;
#[cfg(feature = "swap")]
use alloy_primitives::Address;
#[cfg(feature = "swap")]
use alloy_signer_local::PrivateKeySigner;
#[cfg(feature = "swap")]
use tracing::info;
#[cfg(feature = "swap")]
use vertex_chain::SharedChainProvider;
#[cfg(feature = "swap")]
use vertex_swarm_accounting_swap::service::SwapCommand;
#[cfg(feature = "swap")]
use vertex_swarm_accounting_swap::{SwapEvent, SwapHandle, SwapProvider, SwapService};
#[cfg(feature = "swap")]
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};

use crate::{
    AccountingSettlement, ClientCommand, ClientHandle, ClientService, DEFAULT_PEER_INFLIGHT_CAP,
    PeerInflightLimiter, PeerSelector,
};

/// The concrete shared accounting both client-backed node types build: the
/// default bandwidth accounting wrapped with the config pricer, pinned to the
/// node identity. One instance is shared across the selector, forwarder, client
/// service, and settlement services.
pub type SharedAccounting = Arc<
    ClientAccounting<Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>, FixedPricer<Spec>>,
>;

/// The shared client middle both client-backed entry points assemble: the
/// accounting, the candidate selector, the origin-gated client handle, and the
/// accounting-attached client service.
///
/// Provider-free by design: the chunk provider lives in the native builder
/// (which depends up on this crate) and is also the RPC providers payload, so
/// each entry point builds its own from `origin_handle` and `selector` after
/// calling [`assemble_client_core`]. `enable_forwarding` is likewise the entry
/// point's call: it borrows the node mutably before the node moves into the run
/// loop, so the borrow and the move stay in one scope.
pub struct ClientCore {
    /// The one accounting instance shared across selector, forwarder, service,
    /// and settlement.
    pub accounting: SharedAccounting,
    /// Retrieval and pushsync candidate selection over the shared accounting.
    pub selector: Arc<PeerSelector>,
    /// Per-peer retrieval in-flight cap shared by the chunk provider (reserves
    /// slots) and the service (forgets a peer on disconnect).
    pub inflight: Arc<PeerInflightLimiter>,
    /// The origin-gated client handle the provider dispatches through; its
    /// admission band paces each own request before it sends.
    pub origin_handle: ClientHandle,
    /// The node's topology handle, threaded through unchanged.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The client service with accounting and reporter attached.
    pub client_service: ClientService,
    /// The plain client handle settlement services forward commands to.
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
    /// Bandwidth config the accounting builder consumes.
    pub bandwidth: DefaultBandwidthConfig,
    /// The node's topology handle.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The client service to attach the reporter and in-flight limiter to.
    pub client_service: ClientService,
    /// The plain client handle.
    pub client_handle: ClientHandle,
    /// Soft-accounting settlement, registered first.
    pub pseudosettle_provider: PseudosettleProvider<DefaultBandwidthConfig>,
    /// Native-only settlement providers (swap) registered after pseudosettle.
    pub extra_settlement: Vec<Box<dyn SwarmSettlementProvider>>,
    /// The peer-scoring authority accounting and the service report through.
    pub reporter: Arc<dyn PeerReporter>,
}

/// Assemble the shared client middle: build the accounting with its settlement
/// providers, the selector over it, the origin-gated handle, and the
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
    let accounting = AccountingBuilder::new(bandwidth)
        .with_pricer_from_config(spec)
        .with_settlement(pseudosettle_provider)
        .with_settlements(extra_settlement)
        .build(&identity);
    // One accounting instance is shared by the selector, forwarder, service, and
    // settlement services.
    let accounting: SharedAccounting = Arc::new(accounting);

    // One admission band and settlement trigger shared by the selector and the
    // client service, so the service settles after an own delivery even though it
    // never runs the selector, and both paths share the trigger's in-flight dedup
    // set.
    let admission = accounting.bandwidth().clone();
    let settlement_trigger = Arc::new(AccountingSettlement::new(accounting.bandwidth().clone()));

    let selector = Arc::new(PeerSelector::new(
        Arc::new(topology.clone()),
        admission.clone(),
        Arc::new(accounting.pricing().clone()),
        settlement_trigger.clone(),
    ));

    // The origin-gated handle the chunk provider dispatches through: each
    // own-request leg reserves its price (so `reserved` matches the storer's
    // shadow reserve), bands on the same admission boundary the selector uses,
    // commits the debit on delivery, and releases it on any other exit. The band
    // is the synchronous brake on the outbound rate: an over-threshold request
    // settles or refuses before it sends. The settle trigger is the selector's,
    // so settles dedup across both paths.
    let origin_handle = client_handle.clone().with_origin_gate(
        Arc::new(accounting.pricing().clone()),
        accounting.bandwidth().clone(),
        admission.clone(),
        settlement_trigger.clone(),
    );

    // Per-peer retrieval substream cap: the non-economic overrun guard the chunk
    // provider consults at selection time. One shared instance so a disconnect on
    // the service path forgets the same peer the provider reserves against.
    let inflight = Arc::new(PeerInflightLimiter::new(DEFAULT_PEER_INFLIGHT_CAP));

    // The service reports through the same peer-manager authority accounting uses
    // and forgets a peer's in-flight slots on disconnect. The origin debit is
    // reserved and committed by the dispatch gate on the origin-gated handle, not
    // by the service.
    let client_service = client_service
        .with_reporter(reporter)
        .with_inflight_limiter(Arc::clone(&inflight));

    ClientCore {
        accounting,
        selector,
        inflight,
        origin_handle,
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

/// Resolved swap settlement parameters and the channels that connect the
/// provider, the service, and the node.
///
/// Produced by [`SwapWiring::prepare`] before the accounting is built; consumed
/// by [`SwapWiring::spawn`] after the node command channel exists.
#[cfg(feature = "swap")]
pub struct SwapWiring {
    command_rx: mpsc::UnboundedReceiver<SwapCommand>,
    swap_event_tx: mpsc::UnboundedSender<SwapEvent>,
    swap_event_rx: mpsc::UnboundedReceiver<SwapEvent>,
    signer: Arc<PrivateKeySigner>,
    chequebook: Address,
    beneficiary: Address,
    chain: NamedChain,
    bounce_limit: u128,
}

#[cfg(feature = "swap")]
impl SwapWiring {
    /// Build the swap handle and provider when SWAP settlement is enabled.
    ///
    /// Returns `None` (and leaves accounting swap-free) when `swap_enabled` is
    /// false, or when SWAP is requested but the required chequebook address and
    /// settlement chain cannot be resolved. `beneficiary` defaults to the node
    /// Ethereum address when `None`: the only payout address a cheque sent to us
    /// may name. The returned provider is registered with the accounting builder;
    /// the returned wiring is later handed to [`SwapWiring::spawn`].
    #[allow(clippy::too_many_arguments)]
    pub fn prepare<C>(
        spec: &Arc<Spec>,
        identity: &Arc<Identity>,
        config: &C,
        chequebook: Option<Address>,
        beneficiary: Option<Address>,
        deploy: bool,
        bounce_limit: u128,
        swap_enabled: bool,
    ) -> Option<(SwapProvider<C>, Self)>
    where
        C: SwarmAccountingConfig + Clone + 'static,
    {
        if !swap_enabled {
            return None;
        }

        let Some(chequebook) = chequebook else {
            warn!(
                "SWAP enabled but no chequebook configured; settlement not wired (chequebook deploy not yet supported)"
            );
            return None;
        };
        if deploy {
            warn!(
                "chequebook deploy is not yet supported; using the configured chequebook address"
            );
        }

        let Some(chain) = spec.chain().named() else {
            warn!(
                "SWAP enabled but the network has no named settlement chain; settlement not wired"
            );
            return None;
        };

        // The beneficiary defaults to the node Ethereum address: the only payout
        // address a cheque sent to us may name.
        let beneficiary = beneficiary.unwrap_or_else(|| identity.ethereum_address());

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
            chequebook,
            beneficiary,
            chain,
            bounce_limit,
        };

        Some((provider, wiring))
    }

    /// The sender the node behaviour routes swap wire events into.
    pub fn swap_event_sender(&self) -> mpsc::UnboundedSender<SwapEvent> {
        self.swap_event_tx.clone()
    }

    /// Construct, spawn, and wire the swap service.
    ///
    /// The service records cheque-driven balance changes against `accounting`
    /// (the same instance the provider settles through), drains the provider's
    /// command channel, and consumes routed swap wire events. Cheque violations
    /// are reported through `reporter` so they feed peer scoring. Its
    /// `SendCheque` commands are forwarded to the node through `client_handle`.
    /// With the `swap-chequebook` feature and a connected chain provider, received
    /// cheques are also cashed on chain, paying out to our beneficiary.
    pub fn spawn<A>(
        self,
        executor: &TaskExecutor,
        accounting: Arc<A>,
        client_handle: ClientHandle,
        reporter: Arc<dyn PeerReporter>,
        #[cfg(feature = "swap-chequebook")] chain_provider: Option<&SharedChainProvider>,
        #[cfg(feature = "swap-chequebook")] spec: &Arc<Spec>,
    ) where
        A: SwarmBandwidthAccounting + 'static,
    {
        // The service speaks unbounded `ClientCommand`; the node command channel
        // is bounded and reached through `ClientHandle::send_command`. Bridge the
        // two with a forwarding task so the service never blocks on a full queue.
        let (client_command_tx, client_command_rx) = mpsc::unbounded_channel();
        spawn_client_command_bridge(
            executor,
            "swarm.swap_command_bridge",
            client_command_rx,
            client_handle,
        );

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
        .with_reporter(reporter)
        .with_bounce_limit(alloy_primitives::U256::from(self.bounce_limit));

        #[cfg(feature = "swap-chequebook")]
        let service = attach_cashout(service, chain_provider, spec, self.beneficiary);

        executor.spawn_service("swarm.swap_service", service);
    }
}

/// Attach an on-chain cashout client to the swap service when a chain provider is
/// present, so received cheques are redeemed paying out to our beneficiary.
///
/// The contract address book is resolved here from the spec, since the provider
/// handle carries only the live connection.
#[cfg(feature = "swap-chequebook")]
fn attach_cashout<A, S>(
    service: SwapService<A, S>,
    chain_provider: Option<&SharedChainProvider>,
    spec: &Arc<Spec>,
    beneficiary: Address,
) -> SwapService<A, S>
where
    A: SwarmBandwidthAccounting + 'static,
    S: alloy_signer::SignerSync + Send + Sync + 'static,
{
    use vertex_chain::ChainConfig;
    use vertex_swarm_accounting_swap::cashout::Cashout;

    let Some(provider) = chain_provider else {
        return service;
    };
    let Some(config) = ChainConfig::from_swarm(spec.swarm()) else {
        warn!(
            "chain provider present but the network has no canonical contract deployment; cashout not wired"
        );
        return service;
    };
    let cashout = Cashout::new(provider.provider().clone(), config, beneficiary);
    service.with_cashout(cashout)
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

/// The node run-loop task the launch tail hands back for the entry point to
/// spawn.
///
/// Native: a `Send` [`NodeTaskFn`](vertex_tasks::NodeTaskFn) the builder returns
/// to the binary's task manager. Wasm: a `!Send` sibling the launcher spawns on
/// the browser event loop, since the websocket-transport run future is `!Send`.
#[cfg(not(target_arch = "wasm32"))]
pub type NodeRunTaskFn = vertex_tasks::NodeTaskFn;
#[cfg(target_arch = "wasm32")]
pub type NodeRunTaskFn = Box<
    dyn FnOnce(
        vertex_tasks::GracefulShutdown,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
>;

/// Wrap a future factory as a [`NodeRunTaskFn`] with graceful-shutdown support.
/// The `Send` bound is target-conditional so the wasm node run loop, whose
/// websocket futures are `!Send`, goes through the same helper.
#[cfg(not(target_arch = "wasm32"))]
pub fn single_task<F, Fut>(f: F) -> NodeRunTaskFn
where
    F: FnOnce(vertex_tasks::GracefulShutdown) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Box::new(move |shutdown| Box::pin(f(shutdown)))
}
#[cfg(target_arch = "wasm32")]
pub fn single_task<F, Fut>(f: F) -> NodeRunTaskFn
where
    F: FnOnce(vertex_tasks::GracefulShutdown) -> Fut + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    Box::new(move |shutdown| Box::pin(f(shutdown)))
}

/// A run-task factory: applies multi-hop forwarding (and, for a storer, ingest)
/// over the shared accounting, then returns the node's run-loop task. Keeps the
/// concrete node type out of the shared launch tail.
pub type RunTaskFn =
    Box<dyn FnOnce(SharedAccounting, Arc<dyn PeerReporter>, ClientHandle) -> NodeRunTaskFn>;

/// Node-type-agnostic outputs of node assembly: the topology handle, the client
/// service and handle, and the run-task factory. Every assembly produces these.
pub struct NodeRunParts {
    /// The node's topology handle.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The client service driving the retrieval and pushsync request paths.
    pub client_service: ClientService,
    /// The unthrottled client handle settlement services forward commands to.
    pub client_handle: ClientHandle,
    /// Applies forwarding over the shared accounting and yields the run task.
    pub run: RunTaskFn,
}

/// Network chunk provider wrapped with config-gated download verification: the
/// RPC chunk surface both client entry points expose.
pub type VerifiedChunkProvider = VerifyingChunkProvider<NetworkChunkProvider<Arc<Identity>>>;

/// Outputs of [`build_client_core_tail`]: the run-loop task, the topology handle,
/// the verified chunk provider, the shared accounting and throttled client handle
/// (for an embedder that observes them), and the node-type-specific provider store
/// (`()` for a client, the serve view plus reserve for a storer).
pub struct ClientNodeParts<P> {
    /// The node run-loop task for the entry point to spawn.
    pub task: NodeRunTaskFn,
    /// The node's topology handle.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The selection-aware verified chunk provider.
    pub chunks: VerifiedChunkProvider,
    /// The shared client accounting (selector, throttle, forwarder, service, and
    /// settlement all read this instance).
    pub accounting: SharedAccounting,
    /// The throttled client handle for chunk retrieval and upload.
    pub client: ClientHandle,
    /// Whatever the node type's RPC providers wrap.
    pub provider_store: P,
}

/// The wire-event sinks the node behaviour routes settlement events into,
/// produced by the tail's settlement wiring and threaded into the node build.
pub struct SettlementEventSenders {
    /// Pseudosettle wire events.
    pub pseudosettle: mpsc::UnboundedSender<PseudosettleEvent>,
    /// SWAP wire events, present only when SWAP settlement is wired.
    #[cfg(feature = "swap")]
    pub swap: Option<mpsc::UnboundedSender<SwapEvent>>,
}

/// Resolved SWAP parameters shared by both client entry points.
#[cfg(feature = "swap")]
#[derive(Clone)]
pub struct ClientSwapParams {
    /// `--swap` override; `None` defers to the node type's default.
    pub enable: Option<bool>,
    /// Our chequebook contract address, named in the cheques we issue.
    pub chequebook: Option<Address>,
    /// Payout address received cheques may name; defaults to the identity address.
    pub beneficiary: Option<Address>,
    /// Deploy a new chequebook on startup instead of using an existing one.
    pub deploy: bool,
    /// Per-peer cap on uncashed cheque exposure.
    pub bounce_limit: u128,
}

/// Borrowed, wasm-clean inputs to [`build_client_core_tail`].
pub struct ClientTailParams<'a> {
    /// The runtime node type, which selects the SWAP default and chain need.
    pub node_type: SwarmNodeType,
    /// Network spec for the config pricer and SWAP chain resolution.
    pub spec: &'a Arc<Spec>,
    /// Node identity the accounting, overlay, and SWAP signer are pinned to.
    pub identity: &'a Arc<Identity>,
    /// Bandwidth config driving accounting, pricing, and the self-throttle.
    pub bandwidth: &'a DefaultBandwidthConfig,
    /// Verification checks applied to downloaded chunks.
    pub verify: ChunkVerifyConfig,
    /// SWAP settlement parameters.
    #[cfg(feature = "swap")]
    pub swap: ClientSwapParams,
}

/// Shared client launch tail for the client- and storer-backed node types.
///
/// Wires accounting (violations to the peer manager, SWAP settlement when
/// enabled) and the selection-aware verified chunk provider, then spawns the
/// client and settlement services and the peer-manager tick. `build_node` builds
/// the concrete node over the settlement event sinks and returns its run parts
/// plus the node-type provider store; the tail is agnostic to whether that node
/// is a bare client or a storer. SWAP defaults on for storers and off for
/// clients, overridable through `params.swap.enable`.
///
/// The returned run task is left for the caller to spawn: the native builder
/// hands it to the binary, the embedded launcher spawns it on its executor.
pub async fn build_client_core_tail<P, E, FBuild, Fut>(
    executor: &TaskExecutor,
    params: ClientTailParams<'_>,
    #[cfg(feature = "swap")] chain_provider: Option<SharedChainProvider>,
    build_node: FBuild,
) -> Result<ClientNodeParts<P>, E>
where
    FBuild: FnOnce(SettlementEventSenders) -> Fut,
    Fut: std::future::Future<Output = Result<(NodeRunParts, P), E>>,
{
    // Pseudosettle (soft accounting) is always on: prepare the provider so it
    // embeds in the accounting, and the event sink so wire events route at the
    // node build below.
    let (pseudosettle_provider, pseudosettle_wiring) =
        PseudosettleWiring::prepare(params.bandwidth);
    let pseudosettle_event_sender = pseudosettle_wiring.event_sender();

    // SWAP settlement is prepared next: the provider embeds in the accounting and
    // the swap event sink routes at node build time. The enable decision lives
    // here, once, for both entry points.
    #[cfg(feature = "swap")]
    let (swap_provider, swap_wiring) = {
        let swap_enabled = params
            .swap
            .enable
            .unwrap_or(params.node_type.swap_default());
        SwapWiring::prepare(
            params.spec,
            params.identity,
            params.bandwidth,
            params.swap.chequebook,
            params.swap.beneficiary,
            params.swap.deploy,
            params.swap.bounce_limit,
            swap_enabled,
        )
        .unzip()
    };
    #[cfg(feature = "swap")]
    let swap_event_sender = swap_wiring.as_ref().map(|w| w.swap_event_sender());

    // The concrete node is built over the settlement event sinks: a bare client,
    // or (for a storer) the pullsync-capable node plus its puller. Accounting,
    // selection, and settlement wiring below is identical for both.
    let (
        NodeRunParts {
            topology,
            client_service,
            client_handle,
            run,
        },
        provider_store,
    ) = build_node(SettlementEventSenders {
        pseudosettle: pseudosettle_event_sender,
        #[cfg(feature = "swap")]
        swap: swap_event_sender,
    })
    .await?;

    spawn_peer_manager_task(
        Arc::clone(topology.peer_manager()),
        DEFAULT_TICK_INTERVAL,
        executor,
    );

    // The peer manager is the reporting authority: accounting and the settlement
    // services report violations through it so misbehaving peers are scored down.
    let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

    // SWAP is the only extra provider; pseudosettle is registered first inside
    // the core so soft accounting forgives total debt before SWAP settles.
    let extra_settlement: Vec<Box<dyn SwarmSettlementProvider>> = {
        #[cfg(feature = "swap")]
        {
            swap_provider
                .map(|provider| Box::new(provider) as Box<dyn SwarmSettlementProvider>)
                .into_iter()
                .collect()
        }
        #[cfg(not(feature = "swap"))]
        Vec::new()
    };

    let core = assemble_client_core(ClientCoreCtx {
        spec: Arc::clone(params.spec),
        identity: params.identity.clone(),
        bandwidth: params.bandwidth.clone(),
        topology: topology.clone(),
        client_service,
        client_handle: client_handle.clone(),
        pseudosettle_provider,
        extra_settlement,
        reporter: Arc::clone(&reporter),
    });

    // Multi-hop forwarding plus storer ingest must precede the event loop. The
    // run closure applies both to its concrete node over the shared accounting,
    // then returns the run task. Forwarder relay legs run over the plain handle:
    // the origin gate bands only our own origin retrieval and pushsync.
    let task = (run)(
        Arc::clone(&core.accounting),
        reporter.clone(),
        core.client_handle.clone(),
    );

    let chunk_provider = NetworkChunkProvider::new(core.origin_handle.clone(), topology.clone())
        .with_selector(Arc::clone(&core.selector))
        .with_inflight_limiter(Arc::clone(&core.inflight));
    let chunks = VerifyingChunkProvider::new(chunk_provider, params.verify);

    executor.spawn_service("swarm.client_service", core.client_service);

    // Pseudosettle settlement service over the shared accounting: applies
    // time-based refresh and forwards our outbound settlement to the node.
    pseudosettle_wiring.spawn(
        executor,
        core.accounting.bandwidth().clone(),
        client_handle.clone(),
        Arc::clone(&reporter),
    );

    // SWAP settlement service over the shared accounting: forwards cheque
    // commands to the node and, with a connected chain provider, cashes received
    // cheques on chain.
    #[cfg(feature = "swap")]
    if let Some(wiring) = swap_wiring {
        wiring.spawn(
            executor,
            core.accounting.bandwidth().clone(),
            client_handle,
            Arc::clone(&reporter),
            #[cfg(feature = "swap-chequebook")]
            chain_provider.as_ref(),
            #[cfg(feature = "swap-chequebook")]
            params.spec,
        );
    }

    // The chain provider is kept alive for the node's lifetime by the run task.
    #[cfg(feature = "swap")]
    let task = wrap_with_chain(task, chain_provider);

    Ok(ClientNodeParts {
        task,
        topology,
        chunks,
        accounting: core.accounting,
        client: core.origin_handle,
        provider_store,
    })
}

/// Resolve and validate the shared chain provider for a client- or storer-backed
/// node.
///
/// SWAP is chequebook-based and so always needs the chain; pseudosettle is the
/// chain-free settlement path. Returns `Ok(None)` only for a chain-free node type
/// ([`SwarmNodeType::needs_chain`] is false, i.e. a pseudosettle-only client). A
/// chain-needing node (a storer, or a SWAP-enabled client) that cannot resolve a
/// chain hard-fails with [`NodeChainError::Required`] rather than degrading
/// chainless, whether the cause is no RPC URL, a network with no canonical
/// deployment, or a connection that fails to validate. The construction is
/// target-portable: native TLS or browser fetch transport, picked by `vertex-chain`.
#[cfg(feature = "swap")]
pub async fn node_chain_provider(
    spec: &Arc<Spec>,
    identity: &Arc<Identity>,
    node_type: SwarmNodeType,
    swap_enabled: bool,
    rpc_url: Option<&str>,
) -> Result<Option<SharedChainProvider>, NodeChainError> {
    use vertex_chain::ChainConfig as ChainAddressBook;

    if !node_type.needs_chain(swap_enabled) {
        return Ok(None);
    }

    let Some(rpc_url) = rpc_url else {
        return Err(NodeChainError::Required { node_type });
    };

    // A network with no canonical deployment cannot settle on chain; fail fast
    // before connecting. The address book itself is resolved at the edge by each
    // chain consumer, not carried in the provider handle.
    if ChainAddressBook::from_swarm(spec.swarm()).is_none() {
        return Err(NodeChainError::Required { node_type });
    }

    let signer = (*identity.signer()).clone();
    let provider = vertex_chain::build_chain_provider(rpc_url, signer, spec.chain)
        .await
        .map_err(|e| NodeChainError::Build(e.to_string()))?;

    Ok(Some(provider))
}

/// Failure resolving the chain a chain-needing node requires.
#[cfg(feature = "swap")]
#[derive(Debug, thiserror::Error)]
pub enum NodeChainError {
    /// A chain-needing node type (a storer, a SWAP-enabled client) could not
    /// resolve a chain and may not degrade chainless.
    #[error(
        "node type {node_type} requires an Ethereum chain connection, but none could be resolved: \
         set the chain RPC URL and use a network with a canonical contract deployment"
    )]
    Required {
        /// The node type that hard-failed for want of a chain.
        node_type: SwarmNodeType,
    },
    /// The chain provider could not be constructed or validated.
    #[error("chain provider construction failed: {0}")]
    Build(String),
}

/// Wrap a run task so the chain provider stays alive for the node's lifetime.
#[cfg(feature = "swap")]
fn wrap_with_chain(
    task: NodeRunTaskFn,
    chain_provider: Option<SharedChainProvider>,
) -> NodeRunTaskFn {
    Box::new(move |shutdown| {
        Box::pin(async move {
            let _chain_provider = chain_provider;
            task(shutdown).await;
        })
    })
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

    /// A swap-enabled client registers both settlement providers, pseudosettle
    /// first (soft accounting) and swap second (originated-debt settlement),
    /// matching the order the launch tail composes them in. A chain-free config
    /// is enough: the provider list does not depend on cashout.
    #[cfg(feature = "swap")]
    #[test]
    fn client_accounting_wires_pseudosettle_and_swap() {
        use alloy_primitives::Address;

        let identity = test_identity_arc();
        let config = DefaultBandwidthConfig::default();
        let spec = identity.spec().clone();

        let (pseudosettle_provider, _) = PseudosettleWiring::prepare(&config);
        let (swap_provider, _) = SwapWiring::prepare(
            &spec,
            &identity,
            &config,
            Some(Address::repeat_byte(0xab)),
            None,
            false,
            0,
            true,
        )
        .expect("swap wiring is prepared for a chequebook on a named chain");

        // Compose the accounting exactly as the launch tail does: pseudosettle
        // registered first, swap pushed in through the same extra-settlement seam.
        let accounting = AccountingBuilder::new(config)
            .with_pricer_from_config(spec)
            .with_settlement(pseudosettle_provider)
            .with_settlements(vec![
                Box::new(swap_provider) as Box<dyn SwarmSettlementProvider>
            ])
            .build(&identity);

        assert_eq!(
            accounting.bandwidth().provider_names(),
            vec!["pseudosettle", "swap"],
            "a swap-enabled client reports pseudosettle then swap"
        );
    }

    /// A storer with no RPC URL hard-fails with [`NodeChainError::Required`]
    /// rather than degrade chainless: a storer always needs the chain.
    #[cfg(feature = "swap")]
    #[tokio::test]
    async fn storer_without_chain_config_errors_chain_required() {
        use vertex_swarm_spec::init_dev;

        let spec = init_dev();
        let identity = test_identity_arc();

        let err = node_chain_provider(
            &spec,
            &identity,
            SwarmNodeType::Storer,
            // A storer always needs the chain, so swap_enabled is irrelevant.
            false,
            None,
        )
        .await
        .expect_err("a storer without a chain RPC must hard-fail");
        assert!(
            matches!(
                err,
                NodeChainError::Required {
                    node_type: SwarmNodeType::Storer
                }
            ),
            "a chainless storer must error with Required{{Storer}}, got {err:?}"
        );
    }

    /// A storer on a network with no canonical deployment hard-fails even with a
    /// valid RPC URL: there is no address book to target the contracts.
    #[cfg(feature = "swap")]
    #[tokio::test]
    async fn storer_on_deployment_less_network_errors_chain_required() {
        use vertex_swarm_spec::init_dev;

        let spec = init_dev();
        let identity = test_identity_arc();

        let err = node_chain_provider(
            &spec,
            &identity,
            SwarmNodeType::Storer,
            false,
            Some("https://rpc.example"),
        )
        .await
        .expect_err("a storer on a deployment-less network must hard-fail");
        assert!(
            matches!(
                err,
                NodeChainError::Required {
                    node_type: SwarmNodeType::Storer
                }
            ),
            "a deployment-less storer must error with Required{{Storer}}, got {err:?}"
        );
    }

    /// A pseudosettle-only client does not need a chain, so the provider step
    /// degrades to `Ok(None)` even with no RPC URL configured.
    #[cfg(feature = "swap")]
    #[tokio::test]
    async fn light_client_builds_chainless() {
        use vertex_swarm_spec::init_dev;

        let spec = init_dev();
        let identity = test_identity_arc();

        let provider = node_chain_provider(
            &spec,
            &identity,
            SwarmNodeType::Client,
            // No SWAP: a pseudosettle-only client stays chain-free.
            false,
            None,
        )
        .await
        .expect("a chain-free client must not require a chain");
        assert!(
            provider.is_none(),
            "a pseudosettle-only client degrades chainless, building no provider"
        );
    }

    /// A SWAP-enabled client needs the chain to settle cheques, so a missing RPC
    /// URL hard-fails the same way a storer does.
    #[cfg(feature = "swap")]
    #[tokio::test]
    async fn swap_client_without_chain_config_errors_chain_required() {
        use vertex_swarm_spec::init_dev;

        let spec = init_dev();
        let identity = test_identity_arc();

        let err = node_chain_provider(
            &spec,
            &identity,
            SwarmNodeType::Client,
            // SWAP enabled: the client now needs a chain to settle.
            true,
            None,
        )
        .await
        .expect_err("a SWAP-enabled client without a chain RPC must hard-fail");
        assert!(
            matches!(
                err,
                NodeChainError::Required {
                    node_type: SwarmNodeType::Client
                }
            ),
            "a chainless SWAP client must error with Required{{Client}}, got {err:?}"
        );
    }
}
