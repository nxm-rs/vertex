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
    Accounting, AccountingBuilder, ClientAccounting, DefaultAccountingConfig, FixedPricer,
};
use vertex_swarm_accounting_pseudosettle::{
    PseudosettleCommand, PseudosettleEvent, PseudosettleHandle, PseudosettleProvider,
    PseudosettleService,
};
use vertex_swarm_api::{
    Au, PeerReporter, SwarmAccounting, SwarmAccountingConfig, SwarmClientAccounting, SwarmNodeType,
    SwarmSettlementProvider,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_peer_manager::{DEFAULT_TICK_INTERVAL, spawn_peer_manager_task};
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::TaskExecutor;

use crate::chunks::NetworkChunkProvider;

#[cfg(feature = "swap")]
use crate::args::SwapConfig;
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

use crate::retrieval_latency::RetrievalLatency;
use crate::{
    AccountingSettlement, ClientCommand, ClientHandle, ClientService, DEFAULT_PEER_INFLIGHT_CAP,
    DispatchEngine, PeerInflightLimiter, PeerSelector, RetrievalTopology, SettlementTrigger,
};

/// The concrete shared accounting both client-backed node types build: the
/// default bandwidth accounting wrapped with the config pricer, pinned to the
/// node identity. One instance is shared across the selector, forwarder, client
/// service, and settlement services.
pub type SharedAccounting = Arc<
    ClientAccounting<Arc<Accounting<DefaultAccountingConfig, Arc<Identity>>>, FixedPricer<Spec>>,
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
    /// Per-PO retrieval-latency estimate shared by the service (records) and the
    /// chunk provider (reads to pace its staggered race). Internal mechanism, so
    /// crate-visible rather than part of the re-exported surface.
    pub(crate) retrieval_latency: Arc<RetrievalLatency>,
    /// The origin-gated client handle the provider dispatches through; its
    /// admission band paces each own request before it sends.
    pub origin_handle: ClientHandle,
    /// The settle trigger the origin gate and the retrieval engine share; the
    /// engine drives it to drain a fully-gated peer set. One instance so both
    /// settle paths share the trigger's in-flight dedup.
    pub settlement_trigger: Arc<dyn SettlementTrigger>,
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
///
/// Second phase of the two-phase construction stated at the
/// [`vertex_swarm_accounting`] crate root: the one accounting built from these
/// inputs is shared across the selector, gate, forwarder, and services.
pub struct ClientCoreCtx {
    /// Network spec for the config pricer.
    pub spec: Arc<Spec>,
    /// Node identity the accounting and overlay are pinned to.
    pub identity: Arc<Identity>,
    /// Accounting config the accounting builder consumes.
    pub accounting: DefaultAccountingConfig,
    /// The node's topology handle.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The client service to attach the reporter and in-flight limiter to.
    pub client_service: ClientService,
    /// The plain client handle.
    pub client_handle: ClientHandle,
    /// Soft-accounting settlement, registered first.
    pub pseudosettle_provider: PseudosettleProvider<DefaultAccountingConfig>,
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
        accounting: accounting_config,
        topology,
        client_service,
        client_handle,
        pseudosettle_provider,
        extra_settlement,
        reporter,
    } = ctx;

    // Pseudosettle is registered first so soft accounting forgives total debt
    // before swap settles originated debt; the order matches `settle_all`.
    let accounting = AccountingBuilder::new(accounting_config)
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
    let admission = accounting.accounting().clone();
    let settlement_trigger: Arc<dyn SettlementTrigger> =
        Arc::new(AccountingSettlement::new(accounting.accounting().clone()));

    // Ranking only: the selector triggers no settlement. The origin credit gate
    // settles the peer a request actually dispatches to (`settlement_trigger`),
    // so the settle fan-out is the legs contacted, not the candidate window.
    let selector = Arc::new(PeerSelector::new(
        Arc::new(topology.clone()),
        admission.clone(),
        Arc::new(accounting.pricing().clone()),
    ));

    // The origin-gated handle the chunk provider dispatches through: each
    // own-request leg reserves its price (so `reserved` matches the storer's
    // shadow reserve), bands on the same admission boundary the selector uses,
    // commits the debit on delivery, and releases it on any other exit. The band
    // is the synchronous brake on the outbound rate: an over-threshold request
    // settles or refuses before it sends. This gate is the sole settle trigger:
    // a request settles only the peer it dispatches to, not the whole window.
    let origin_handle = client_handle.clone().with_origin_gate(
        Arc::new(accounting.pricing().clone()),
        accounting.accounting().clone(),
        settlement_trigger.clone(),
    );

    // Per-peer retrieval substream cap: the non-economic overrun guard the chunk
    // provider consults at selection time. One shared instance so a disconnect on
    // the service path forgets the same peer the provider reserves against.
    let inflight = Arc::new(PeerInflightLimiter::new(DEFAULT_PEER_INFLIGHT_CAP));

    // Per-PO retrieval-latency estimate shared between the service (records a
    // completed originated retrieval) and the chunk provider (reads it to pace
    // the staggered race). One instance so the provider hedges on what the
    // service has observed.
    let retrieval_latency = Arc::new(RetrievalLatency::new());

    // The service reports through the same peer-manager authority accounting uses
    // and forgets a peer's in-flight slots on disconnect. The origin debit is
    // reserved and committed by the dispatch gate on the origin-gated handle, not
    // by the service.
    let client_service = client_service
        .with_reporter(reporter)
        .with_inflight_limiter(Arc::clone(&inflight))
        .with_retrieval_latency(Arc::clone(&retrieval_latency));

    ClientCore {
        accounting,
        selector,
        inflight,
        retrieval_latency,
        origin_handle,
        settlement_trigger,
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
        A: SwarmAccounting + 'static,
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
    pub fn prepare<C>(
        spec: &Arc<Spec>,
        identity: &Arc<Identity>,
        config: &C,
        swap: &SwapConfig,
        swap_enabled: bool,
    ) -> Option<(SwapProvider<C>, Self)>
    where
        C: SwarmAccountingConfig + Clone + 'static,
    {
        if !swap_enabled {
            return None;
        }

        let Some(chequebook) = swap.chequebook else {
            warn!(
                "SWAP enabled but no chequebook configured; settlement not wired (chequebook deploy not yet supported)"
            );
            return None;
        };
        if swap.deploy {
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
        let beneficiary = swap
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
            chequebook,
            beneficiary,
            chain,
            bounce_limit: swap.bounce_limit,
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
        A: SwarmAccounting + 'static,
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
    A: SwarmAccounting + 'static,
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
                    // Backpressure, never drop: a dropped settle command strands
                    // the originating service's per-peer pending entry, so that
                    // peer never settles again and is eventually dropped at its
                    // line. Awaiting the bounded node channel buffers the wait on
                    // the upstream service channel, which the service's
                    // one-settle-per-peer dedup keeps bounded. Errors only on a
                    // closed channel (node shutting down).
                    if let Err(e) = client_handle.send_command_buffered(command).await {
                        warn!(error = %e, "settlement command bridge stopped: node channel closed");
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

/// Tail-built shared components a node assembly applies to its concrete node
/// (forwarding, and for a storer ingest) before returning the run-loop task.
pub struct AssemblyContext {
    /// The one shared accounting instance; the run task keeps it alive.
    pub accounting: SharedAccounting,
    /// The fully-wired dispatch engine, applied as the node's relay role.
    pub engine: NativeDispatchEngine,
}

/// A run-task factory: applies multi-hop forwarding (and, for a storer, ingest)
/// over the shared accounting and the engine's relay role, then returns the
/// node's run-loop task. Keeps the concrete node type out of the shared launch
/// tail.
pub type RunTaskFn = Box<dyn FnOnce(AssemblyContext) -> NodeRunTaskFn>;

/// The native client's fully-capable dispatch engine instantiation, shared by
/// the chunk provider (origin dispatch) and the forwarder (relay role).
pub type NativeDispatchEngine =
    DispatchEngine<Arc<PeerSelector>, Arc<PeerInflightLimiter>, Arc<RetrievalLatency>>;

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

/// The native client's fully-capable provider instantiation: score/affordability
/// ordering, the per-peer in-flight cap, and the per-PO latency estimate. This is
/// the RPC chunk surface both client entry points expose; content integrity is
/// enforced during retrieval decode, so no download-side wrapper sits over it.
pub type NativeChunkProvider =
    NetworkChunkProvider<Arc<PeerSelector>, Arc<PeerInflightLimiter>, Arc<RetrievalLatency>>;

/// Outputs of [`ClientCoreTail::finish`]: the run-loop task, the topology handle,
/// the chunk provider, the shared accounting and throttled client handle
/// (for an embedder that observes them), and the node-type-specific provider store
/// (`()` for a client, the serve view plus reserve for a storer).
pub struct ClientNodeParts<P> {
    /// The node run-loop task for the entry point to spawn.
    pub task: NodeRunTaskFn,
    /// The node's topology handle.
    pub topology: TopologyHandle<Arc<Identity>>,
    /// The selection-aware chunk provider.
    pub chunks: NativeChunkProvider,
    /// The per-peer retrieval in-flight limiter, shared with the service that
    /// forgets a peer on disconnect. Exposed so an embedder driving its own
    /// engine (the browser provider) caps against the same instance.
    pub inflight: Arc<PeerInflightLimiter>,
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

/// Borrowed, wasm-clean inputs to [`ClientCoreTail::prepare`].
pub struct ClientTailParams<'a> {
    /// The runtime node type, which selects the SWAP default and chain need.
    pub node_type: SwarmNodeType,
    /// Network spec for the config pricer and SWAP chain resolution.
    pub spec: &'a Arc<Spec>,
    /// Node identity the accounting, overlay, and SWAP signer are pinned to.
    pub identity: &'a Arc<Identity>,
    /// Accounting config driving the ledger, pricing, and the self-throttle.
    pub accounting: &'a DefaultAccountingConfig,
    /// SWAP settlement configuration.
    #[cfg(feature = "swap")]
    pub swap: &'a SwapConfig,
}

/// Shared client launch tail for the client- and storer-backed node types.
///
/// Splits assembly into two infallible synchronous phases around the concrete
/// node build. [`ClientCoreTail::prepare`] wires the settlement providers into
/// the accounting and yields the event sinks the node build routes wire events
/// into; [`ClientCoreTail::finish`] takes the built node's run parts, wires the
/// selection-aware chunk provider and the shared accounting, spawns the client
/// and settlement services and the peer-manager tick, and returns the run parts
/// for the caller to spawn. SWAP defaults on for storers and off for clients,
/// overridable through `params.swap.enable`.
pub struct ClientCoreTail {
    spec: Arc<Spec>,
    identity: Arc<Identity>,
    accounting: DefaultAccountingConfig,
    pseudosettle_provider: PseudosettleProvider<DefaultAccountingConfig>,
    pseudosettle_wiring: PseudosettleWiring,
    #[cfg(feature = "swap")]
    swap: Option<(SwapProvider<DefaultAccountingConfig>, SwapWiring)>,
    #[cfg(feature = "swap")]
    chain_provider: Option<SharedChainProvider>,
}

impl ClientCoreTail {
    /// Prepare the settlement wiring and return the event sinks the node build
    /// routes wire events into.
    ///
    /// Pseudosettle (soft accounting) is always wired; SWAP is wired when enabled
    /// (defaulting on for storers, off for clients). The enable decision lives
    /// here, once, for both entry points.
    pub fn prepare(
        params: ClientTailParams<'_>,
        #[cfg(feature = "swap")] chain_provider: Option<SharedChainProvider>,
    ) -> (Self, SettlementEventSenders) {
        // Pseudosettle: prepare the provider so it embeds in the accounting, and
        // the event sink so wire events route at the node build.
        let (pseudosettle_provider, pseudosettle_wiring) =
            PseudosettleWiring::prepare(params.accounting);
        let pseudosettle_event_sender = pseudosettle_wiring.event_sender();

        // SWAP: the provider embeds in the accounting and the swap event sink
        // routes at node build time.
        #[cfg(feature = "swap")]
        let swap = {
            let swap_enabled = params
                .swap
                .enable
                .unwrap_or(params.node_type.swap_default());
            SwapWiring::prepare(
                params.spec,
                params.identity,
                params.accounting,
                params.swap,
                swap_enabled,
            )
        };
        #[cfg(feature = "swap")]
        let swap_event_sender = swap.as_ref().map(|(_, wiring)| wiring.swap_event_sender());

        let senders = SettlementEventSenders {
            pseudosettle: pseudosettle_event_sender,
            #[cfg(feature = "swap")]
            swap: swap_event_sender,
        };

        let tail = Self {
            spec: Arc::clone(params.spec),
            identity: params.identity.clone(),
            accounting: params.accounting.clone(),
            pseudosettle_provider,
            pseudosettle_wiring,
            #[cfg(feature = "swap")]
            swap,
            #[cfg(feature = "swap")]
            chain_provider,
        };

        (tail, senders)
    }

    /// Finish assembly over the built node's run parts: wire the shared accounting
    /// and the selection-aware chunk provider, spawn the client and settlement
    /// services and the peer-manager tick, and return the run parts for the caller
    /// to spawn.
    pub fn finish<P>(
        self,
        executor: &TaskExecutor,
        parts: NodeRunParts,
        provider_store: P,
    ) -> ClientNodeParts<P> {
        let NodeRunParts {
            topology,
            client_service,
            client_handle,
            run,
        } = parts;

        // The provider reads the node's own cache before racing the swarm; it is
        // the same store the service caches deliveries into and the handler serves
        // from. Read before the service moves into the core.
        let provider_cache = client_service.store();

        spawn_peer_manager_task(
            Arc::clone(topology.peer_manager()),
            DEFAULT_TICK_INTERVAL,
            executor,
        );

        // The peer manager is the reporting authority: accounting and the
        // settlement services report violations through it so misbehaving peers
        // are scored down.
        let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

        #[cfg(feature = "swap")]
        let (swap_provider, swap_wiring) = self.swap.unzip();

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
            spec: Arc::clone(&self.spec),
            identity: self.identity.clone(),
            accounting: self.accounting.clone(),
            topology: topology.clone(),
            client_service,
            client_handle: client_handle.clone(),
            pseudosettle_provider: self.pseudosettle_provider,
            extra_settlement,
            reporter: Arc::clone(&reporter),
        });

        // One dispatch engine for every origin and relay path. The routing table's
        // max bin is a spec constant; read it once here and hand it to the engine as
        // a field rather than a per-request topology query. Origin dispatch runs
        // over the gated origin handle; relay legs debit through the forwarder's
        // two-leg accounting instead (`originated = false`), so the shared handle is
        // the origin-gated one and the gate simply never fires for relays.
        let max_bin = topology.max_bin();
        let engine = DispatchEngine::new(
            core.origin_handle.clone(),
            Arc::new(topology.clone()) as Arc<dyn RetrievalTopology>,
            max_bin,
            Arc::clone(&core.selector),
            Arc::clone(&core.inflight),
            Arc::clone(&core.retrieval_latency),
            Arc::clone(&core.settlement_trigger),
        );

        // Multi-hop forwarding plus storer ingest must precede the event loop. The
        // run closure applies both to its concrete node over the shared accounting
        // and the engine's relay role, then returns the run task.
        let task = (run)(AssemblyContext {
            accounting: Arc::clone(&core.accounting),
            engine: engine.clone(),
        });

        let chunks = NetworkChunkProvider::new(engine, provider_cache);

        executor.spawn_service("swarm.client_service", core.client_service);

        // Pseudosettle settlement service over the shared accounting: applies
        // time-based refresh and forwards our outbound settlement to the node.
        self.pseudosettle_wiring.spawn(
            executor,
            core.accounting.accounting().clone(),
            client_handle.clone(),
            Arc::clone(&reporter),
        );

        // SWAP settlement service over the shared accounting: forwards cheque
        // commands to the node and, with a connected chain provider, cashes
        // received cheques on chain.
        #[cfg(feature = "swap")]
        if let Some(wiring) = swap_wiring {
            wiring.spawn(
                executor,
                core.accounting.accounting().clone(),
                client_handle,
                Arc::clone(&reporter),
                #[cfg(feature = "swap-chequebook")]
                self.chain_provider.as_ref(),
                #[cfg(feature = "swap-chequebook")]
                &self.spec,
            );
        }

        // The chain provider is kept alive for the node's lifetime by the run task.
        #[cfg(feature = "swap")]
        let task = wrap_with_chain(task, self.chain_provider);

        ClientNodeParts {
            task,
            topology,
            chunks,
            inflight: core.inflight,
            accounting: core.accounting,
            client: core.origin_handle,
            provider_store,
        }
    }
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
        let config = DefaultAccountingConfig::default();

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
            accounting.accounting().provider_names(),
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
        let config = DefaultAccountingConfig::default();
        let spec = identity.spec().clone();

        let (pseudosettle_provider, _) = PseudosettleWiring::prepare(&config);
        let swap_config = SwapConfig {
            enable: Some(true),
            chequebook: Some(Address::repeat_byte(0xab)),
            beneficiary: None,
            deploy: false,
            bounce_limit: 0,
        };
        let (swap_provider, _) = SwapWiring::prepare(&spec, &identity, &config, &swap_config, true)
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
            accounting.accounting().provider_names(),
            vec!["pseudosettle", "swap"],
            "a swap-enabled client reports pseudosettle then swap"
        );
    }

    /// The tail's prepare phase leaves SWAP unwired for a default client (swap
    /// defaults off) and for an enable-forced client with no chequebook (warn and
    /// degrade), so the returned event sinks carry no swap sender.
    #[cfg(feature = "swap")]
    #[test]
    fn prepare_leaves_swap_unwired_without_chequebook() {
        let identity = test_identity_arc();
        let spec = identity.spec().clone();
        let accounting = DefaultAccountingConfig::default();

        // A default client leaves SWAP off (swap_default is off for clients).
        let swap_off = SwapConfig::default();
        let (_tail, senders) = ClientCoreTail::prepare(
            ClientTailParams {
                node_type: SwarmNodeType::Client,
                spec: &spec,
                identity: &identity,
                accounting: &accounting,
                swap: &swap_off,
            },
            None,
        );
        assert!(
            senders.swap.is_none(),
            "a default client leaves swap unwired"
        );

        // Enable forced on, but no chequebook: the swap wiring warns and degrades,
        // so still no swap sender.
        let swap_no_chequebook = SwapConfig {
            enable: Some(true),
            ..Default::default()
        };
        let (_tail, senders) = ClientCoreTail::prepare(
            ClientTailParams {
                node_type: SwarmNodeType::Client,
                spec: &spec,
                identity: &identity,
                accounting: &accounting,
                swap: &swap_no_chequebook,
            },
            None,
        );
        assert!(
            senders.swap.is_none(),
            "an enable-forced client with no chequebook degrades swap-free"
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
