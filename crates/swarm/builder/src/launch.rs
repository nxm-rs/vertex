//! SwarmLaunchConfig implementations for config types.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use vertex_net_peer_store::PeerSnapshotStore;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_accounting::{Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer};
#[cfg(feature = "chain")]
use vertex_swarm_api::SwarmSpec;
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, PeerReporter, StorerComponents, SwarmClientAccounting,
    SwarmLaunchConfig, SwarmNodeType, construct,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_node::{
    BootNode, ClientCoreCtx, ClientNode, PseudosettleWiring, SharedAccounting, assemble_client_core,
};
use vertex_swarm_peer_manager::{
    DEFAULT_TICK_INTERVAL, DbPeerSnapshotStore, PeerSnapshot, spawn_peer_manager_task,
};
use vertex_swarm_postage::DbBatchStore;
use vertex_swarm_spec::{Loggable, Spec};
use vertex_swarm_storer::DbReserve;
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::{GracefulShutdown, NodeTaskFn};

use crate::config::{BootnodeConfig, ClientConfig, StorerConfig};
use crate::error::SwarmNodeError;
use crate::providers::NetworkChunkProvider;
use crate::verify::{ChunkVerifyConfig, VerifyingChunkProvider};

/// Network chunk provider wrapped with config-gated download verification.
type VerifiedChunkProvider = VerifyingChunkProvider<NetworkChunkProvider<Arc<Identity>>>;

#[cfg(feature = "swap")]
use vertex_swarm_node::SwapWiring;
#[cfg(feature = "chain")]
use vertex_swarm_node::args::ChainConfig;
#[cfg(feature = "swap")]
use vertex_swarm_node::args::SwapConfig;

#[cfg(feature = "chain")]
use crate::chain::SharedChainProvider;

type PeerStore = Arc<dyn PeerSnapshotStore<PeerSnapshot>>;

/// Stats collection interval for database metrics.
const DB_METRICS_INTERVAL: Duration = Duration::from_secs(30);

fn log_build_start(node_type: SwarmNodeType, spec: &Spec) {
    info!(%node_type, "Building node...");
    spec.log();
}

/// Wrap a future factory as a NodeTaskFn with graceful shutdown support.
fn single_task<F, Fut>(f: F) -> NodeTaskFn
where
    F: FnOnce(GracefulShutdown) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Box::new(move |shutdown| Box::pin(f(shutdown)))
}

/// Open the shared database when persistence is configured.
///
/// `ctx.db_path()` selects the storage mode: `None` runs fully in-memory,
/// `Some(path)` opens (or creates) the file and spawns the metrics task. An open
/// failure on a configured path degrades to in-memory rather than aborting.
fn open_shared_database(ctx: &dyn InfrastructureContext) -> Option<Arc<RedbDatabase>> {
    let Some(path) = ctx.db_path() else {
        info!("Node storage: in-memory (opt into persistence with --db.persist or --db.path)");
        return None;
    };
    match vertex_storage_redb::open_database(Some(path), false) {
        Ok(db) => {
            info!(path = %path.display(), "Node storage: persistent");
            spawn_db_metrics_task(ctx, db.clone());
            Some(db)
        }
        Err(e) => {
            warn!(
                error = %e,
                path = %path.display(),
                "Failed to open database at configured path; degrading to in-memory storage, \
                 peer snapshots will not be persisted and known peers are lost on shutdown"
            );
            None
        }
    }
}

fn spawn_db_metrics_task(ctx: &dyn InfrastructureContext, db: Arc<RedbDatabase>) {
    ctx.executor()
        .spawn_with_graceful_shutdown_signal("db.metrics", move |shutdown| async move {
            let mut shutdown = std::pin::pin!(shutdown);
            let mut interval = vertex_tasks::time::interval(DB_METRICS_INTERVAL);

            loop {
                tokio::select! {
                    guard = &mut shutdown => {
                        tracing::debug!("db metrics task shutting down");
                        drop(guard);
                        break;
                    }
                    _ = interval.tick() => {
                        vertex_storage_redb::stats::collect_db_metrics(&db);
                    }
                }
            }
        });
}

/// Build and validate the shared chain provider for a node.
///
/// Returns `Ok(None)` only for a chain-free node type ([`SwarmNodeType::needs_chain`]
/// is false). A chain-needing node that cannot resolve a chain hard-fails with
/// [`SwarmNodeError::ChainRequired`] rather than degrading chainless, whether the
/// cause is no `--chain.rpc-url`, a network with no canonical deployment, or a
/// connection that fails to validate. The returned [`SharedChainProvider`] is a
/// cloneable handle, not a spawned service.
#[cfg(feature = "chain")]
async fn build_node_chain_provider(
    spec: &Arc<Spec>,
    identity: &Arc<Identity>,
    node_type: SwarmNodeType,
    swap_enabled: bool,
    chain: &ChainConfig,
) -> Result<Option<SharedChainProvider>, SwarmNodeError> {
    use vertex_chain::ChainConfig as ChainAddressBook;
    use vertex_swarm_api::SwarmIdentity;

    if !node_type.needs_chain(swap_enabled) {
        return Ok(None);
    }

    let Some(rpc_url) = chain.rpc_url.as_deref() else {
        tracing::debug!(
            %node_type,
            "chain required but no --chain.rpc-url configured"
        );
        return Err(SwarmNodeError::ChainRequired { node_type });
    };

    // A network with no canonical deployment cannot settle on chain; fail fast
    // before connecting. The address book itself is resolved at the edge by each
    // chain consumer, not carried in the provider handle.
    if ChainAddressBook::from_swarm(spec.swarm()).is_none() {
        tracing::debug!(
            %node_type,
            "chain required but the network has no canonical contract deployment"
        );
        return Err(SwarmNodeError::ChainRequired { node_type });
    }

    let signer = (*identity.signer()).clone();
    let provider = crate::chain::build_chain_provider(rpc_url, signer, spec.chain)
        .await
        .map_err(|e| SwarmNodeError::Chain(e.to_string()))?;

    Ok(Some(provider))
}

fn create_peer_store(db: &Option<Arc<RedbDatabase>>) -> Option<PeerStore> {
    let db = db.as_ref()?;
    let store = Arc::new(DbPeerSnapshotStore::new(db.clone()));
    match store.init() {
        Ok(()) => {
            info!("Peer snapshot store: shared database");
            Some(store as PeerStore)
        }
        Err(e) => {
            warn!(error = %e, "Failed to init peer snapshot table");
            None
        }
    }
}

macro_rules! define_launch_types {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        pub struct $name;

        impl vertex_swarm_api::SwarmPrimitives for $name {
            type Spec = Arc<Spec>;
            type Identity = Arc<Identity>;
        }

        impl vertex_swarm_api::SwarmNetworkTypes for $name {
            type Topology = TopologyHandle<Arc<Identity>>;
        }
    };
    ($(#[$attr:meta])* $name:ident, with_client) => {
        define_launch_types!($(#[$attr])* $name);

        impl vertex_swarm_api::SwarmClientTypes for $name {
            type Accounting = ClientAccounting<
                Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>,
                FixedPricer<Arc<Spec>>,
            >;
        }
    };
}

define_launch_types!(
    /// Bootnode launch types: spec, identity, topology, no accounting.
    BootnodeLaunchTypes
);
define_launch_types!(
    /// Client launch types: bootnode types plus the default accounting stack.
    ClientLaunchTypes,
    with_client
);
define_launch_types!(
    /// Storer launch types: bootnode types plus the default accounting stack.
    StorerLaunchTypes,
    with_client
);

/// Turns the opened shared database (if any) into the node's `SwarmLocalStore`.
///
/// Each node type wires its own: the client ignores the handle and builds an
/// in-memory cache, the storer builds the persisting reserve over the same
/// database the peer store backs onto (one handle, not two opens).
type StoreFactory<'a> =
    Box<dyn FnOnce(Option<Arc<RedbDatabase>>) -> Result<NodeStore, SwarmNodeError> + Send + 'a>;

/// Guard message: a storer with no reserve is an internal wiring bug, not config.
const STORER_RESERVE_MISSING: &str = "storer reserve missing";

/// The node's local store, plus the storer reserve view when the node is a storer.
///
/// `local` is the retrieval-serve view: a client's in-memory cache, or a storer's
/// [`composite::CacheThenReserve`] layering the cache over the reserve (reserve
/// wins on overlap). `reserve` is the separate storer reserve view, erased to
/// [`BinCursorStore`](vertex_swarm_api::BinCursorStore) and carried only for a
/// storer; it feeds pushsync ingest (upcast to `Arc<dyn ReserveStore>` at the
/// `enable_storage` call site) and the served reserve capabilities. A client
/// leaves `reserve` `None`.
struct NodeStore {
    local: Arc<dyn vertex_swarm_api::SwarmLocalStore>,
    reserve: Option<Arc<dyn vertex_swarm_api::BinCursorStore>>,
    /// The reserve as the pullsync server snapshot, carried only for a storer
    /// whose reserve is the default [`DbReserve`]. A reserve seam that is not a
    /// `DbReserve` leaves this `None`, so pullsync inbound serving is skipped.
    pullsync: Option<Arc<dyn vertex_swarm_api::PullStorage>>,
    /// A second handle onto the reserve's batch set, carried only for the default
    /// [`DbReserve`], so the puller's funding verifier reads the same batches the
    /// reserve admits against. A reserve seam leaves this `None` and the puller
    /// falls back to the signature-only verifier.
    batches: Option<DbBatchStore<RedbDatabase>>,
}

/// A cache override supplied through the builder. With no seam the launch path
/// builds the default in-memory [`vertex_swarm_localstore::ChunkStore`] sized
/// from the local-store config.
pub(crate) enum CacheSeam {
    /// A pre-built cache, used as-is.
    Ready(Arc<dyn vertex_swarm_api::SwarmLocalStore>),
    /// A factory invoked at build time with the opened shared database.
    Factory(CacheFactory),
}

/// A reserve override supplied through the builder. With no seam the storer
/// launch path builds the default admission-gated [`DbReserve`] over the shared
/// database.
pub(crate) enum ReserveSeam {
    /// A pre-built reserve, used as-is.
    Ready(Arc<dyn vertex_swarm_api::BinCursorStore>),
    /// A factory invoked at build time with the opened shared database.
    Factory(ReserveFactory),
}

/// Builds a cache from the opened shared database (if any).
pub(crate) type CacheFactory = Box<
    dyn FnOnce(
            Option<Arc<RedbDatabase>>,
        ) -> Result<Arc<dyn vertex_swarm_api::SwarmLocalStore>, SwarmNodeError>
        + Send,
>;

/// Builds a reserve from the opened shared database (if any).
pub(crate) type ReserveFactory = Box<
    dyn FnOnce(
            Option<Arc<RedbDatabase>>,
        ) -> Result<Arc<dyn vertex_swarm_api::BinCursorStore>, SwarmNodeError>
        + Send,
>;

/// Borrowed inputs for [`build_client_backed_node`], gathered from a validated
/// client or storer config.
struct ClientNodeParams<'a> {
    node_type: SwarmNodeType,
    spec: &'a Arc<Spec>,
    identity: &'a Arc<Identity>,
    network: &'a NetworkConfig<KademliaConfig>,
    bandwidth: &'a DefaultBandwidthConfig,
    verify: ChunkVerifyConfig,
    /// Builds the node's local store from the opened shared database.
    make_store: StoreFactory<'a>,
    #[cfg(feature = "chain")]
    chain: &'a ChainConfig,
    #[cfg(feature = "swap")]
    swap: &'a SwapConfig,
}

/// Outputs of [`build_client_backed_node`]: the node task plus the handles the
/// node-type-specific RPC providers wrap.
struct ClientNodeParts {
    task: NodeTaskFn,
    topology: TopologyHandle<Arc<Identity>>,
    chunks: VerifiedChunkProvider,
    /// The node's local store, erased to the trait; the storer wires this same
    /// instance into its components so retrieval and storage share one store.
    store: Arc<dyn vertex_swarm_api::SwarmLocalStore>,
    /// The storer reserve, erased to [`BinCursorStore`]; `None` for a client. The
    /// storer wires this same instance so its components and the run loop share
    /// one reserve.
    reserve: Option<Arc<dyn vertex_swarm_api::BinCursorStore>>,
}

/// Shared launch path for the client- and storer-backed node types.
///
/// Wires accounting (violations to the peer manager, SWAP settlement when
/// enabled) and the selection-aware verified chunk provider, then spawns the run
/// loop in a task owning the accounting and chain handles for the node's lifetime.
async fn build_client_backed_node(
    ctx: &dyn InfrastructureContext,
    params: ClientNodeParams<'_>,
) -> Result<ClientNodeParts, SwarmNodeError> {
    let node_type = params.node_type;
    log_build_start(node_type, params.spec);

    // Default allowances follow node type. A non-storer advertises no storage in
    // its handshake, so the reference network meters it with the light figures
    // (payment threshold and refresh rate divided by the light factor). We size
    // our own threshold and refresh rate to that same ceiling so we settle and
    // self-throttle below the light disconnect limit the remote enforces on us,
    // rather than pacing against the wider storer figures and being dropped
    // before our own accounting engages. A storer keeps the full figures.
    let bandwidth = if node_type.requires_storage() {
        params.bandwidth.clone()
    } else {
        params.bandwidth.clone().light()
    };

    // SWAP defaults on for storers (maximum support) and off for clients; an
    // explicit --swap overrides. Resolved once and shared with the chain check.
    #[cfg(feature = "swap")]
    let swap_enabled = params.swap.enable.unwrap_or(node_type.swap_default());
    #[cfg(not(feature = "swap"))]
    let swap_enabled = false;

    // A storer always needs a chain (staking, oracle, settlement); a client needs
    // one only with SWAP. Resolve this precondition before any runtime work so a
    // chain-required node fails before allocating tasks, the database, or the node.
    #[cfg(feature = "chain")]
    let chain_provider = build_node_chain_provider(
        params.spec,
        params.identity,
        node_type,
        swap_enabled,
        params.chain,
    )
    .await?;
    // Without the `chain` feature a chain-required node cannot resolve one.
    #[cfg(not(feature = "chain"))]
    if node_type.needs_chain(swap_enabled) {
        return Err(SwarmNodeError::ChainRequired { node_type });
    }

    let db = open_shared_database(ctx);
    let peer_store = create_peer_store(&db);

    let NodeStore {
        local: store,
        reserve,
        pullsync,
        batches,
    } = (params.make_store)(db.clone())?;
    let node_store = Arc::clone(&store);

    // Pseudosettle (soft accounting) is always on for client and storer nodes:
    // prepare the provider so it embeds in the accounting, and the event sink so
    // pseudosettle wire events route at node build time.
    let (pseudosettle_provider, pseudosettle_wiring) = PseudosettleWiring::prepare(&bandwidth);
    let pseudosettle_event_sender = pseudosettle_wiring.event_sender();

    // SWAP settlement is prepared next: the provider embeds in the accounting
    // and the swap event sink routes at node build time.
    #[cfg(feature = "swap")]
    let (swap_provider, swap_wiring) = SwapWiring::prepare(
        params.spec,
        params.identity,
        &bandwidth,
        params.swap.chequebook,
        params.swap.beneficiary,
        params.swap.deploy,
        params.swap.bounce_limit,
        swap_enabled,
    )
    .unzip();
    #[cfg(feature = "swap")]
    let swap_event_sender = swap_wiring.as_ref().map(|w| w.swap_event_sender());

    // A storer runs the pullsync protocol over a `StorerBehaviour`; every other
    // client-backed node runs the bare client behaviour. The branch differs only
    // in node assembly and the optional puller spawn; accounting, selection, and
    // settlement wiring below is identical.
    let StorerCapable {
        topology,
        client_service,
        client_handle,
        run,
    } = if node_type == SwarmNodeType::Storer {
        assemble_storer_node(
            ctx,
            params.identity,
            params.network,
            node_store,
            peer_store,
            db.clone(),
            reserve.clone(),
            pullsync,
            batches,
            pseudosettle_event_sender,
            #[cfg(feature = "swap")]
            swap_event_sender,
        )
        .await?
    } else {
        assemble_client_node(
            params.identity,
            params.network,
            node_store,
            peer_store,
            pseudosettle_event_sender,
            #[cfg(feature = "swap")]
            swap_event_sender,
        )
        .await?
    };

    spawn_peer_manager_task(
        Arc::clone(topology.peer_manager()),
        DEFAULT_TICK_INTERVAL,
        ctx.executor(),
    );

    // The peer manager is the reporting authority: accounting and the settlement
    // services report violations through it so misbehaving peers are scored down.
    let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

    // SWAP is the only native-only provider; pseudosettle is registered first
    // inside the core so soft accounting forgives total debt before SWAP settles.
    let extra_settlement: Vec<Box<dyn vertex_swarm_api::SwarmSettlementProvider>> = {
        #[cfg(feature = "swap")]
        {
            swap_provider
                .map(|provider| {
                    Box::new(provider) as Box<dyn vertex_swarm_api::SwarmSettlementProvider>
                })
                .into_iter()
                .collect()
        }
        #[cfg(not(feature = "swap"))]
        Vec::new()
    };

    // Assemble the shared client middle (accounting, selector, throttle, service)
    // once; both client entry points wire the same instances through this.
    let core = assemble_client_core(ClientCoreCtx {
        spec: Arc::clone(params.spec),
        identity: params.identity.clone(),
        bandwidth,
        topology: topology.clone(),
        client_service,
        client_handle: client_handle.clone(),
        pseudosettle_provider,
        extra_settlement,
        reporter: Arc::clone(&reporter),
    });

    // Multi-hop forwarding plus storer ingest must precede the event loop. The
    // run closure applies both to its concrete node over the shared accounting,
    // then returns the run task. The forwarder relay legs run over the
    // unthrottled handle: the self-throttle paces only our own origin retrieval
    // and pushsync, never chunks we relay on another peer's behalf.
    let task = (run)(
        Arc::clone(&core.accounting),
        reporter.clone(),
        core.client_handle.clone(),
    );

    let chunk_provider = NetworkChunkProvider::new(core.throttled_handle.clone(), topology.clone())
        .with_selector(Arc::clone(&core.selector));
    let chunks = VerifyingChunkProvider::new(chunk_provider, params.verify);

    ctx.executor()
        .spawn_service("swarm.client_service", core.client_service);

    // Pseudosettle settlement service over the shared accounting: applies
    // time-based refresh and forwards our outbound settlement to the node.
    pseudosettle_wiring.spawn(
        ctx.executor(),
        core.accounting.bandwidth().clone(),
        client_handle.clone(),
        Arc::clone(&reporter),
    );

    // SWAP settlement service over the shared accounting: forwards cheque
    // commands to the node and cashes received cheques on chain when a provider
    // is present.
    #[cfg(feature = "swap")]
    if let Some(wiring) = swap_wiring {
        wiring.spawn(
            ctx.executor(),
            core.accounting.bandwidth().clone(),
            client_handle,
            Arc::clone(&reporter),
            #[cfg(feature = "chain")]
            chain_provider.as_ref(),
            #[cfg(feature = "chain")]
            params.spec,
        );
    }

    // The chain provider is kept alive for the node's lifetime by the run task.
    #[cfg(feature = "chain")]
    let task = wrap_with_chain(task, chain_provider);

    info!(%node_type, "Node built successfully");
    Ok(ClientNodeParts {
        task,
        topology,
        chunks,
        store,
        reserve,
    })
}

/// A run-task factory: applies multi-hop forwarding (and storer ingest) over the
/// shared accounting, then returns the node's event-loop task. The factory keeps
/// the concrete node type out of the shared launch tail.
type RunTaskFn = Box<
    dyn FnOnce(
        SharedAccounting,
        Arc<dyn PeerReporter>,
        vertex_swarm_node::ClientHandle,
    ) -> NodeTaskFn,
>;

/// Node-type-agnostic outputs of node assembly: the topology handle, the client
/// service and handle, and the run-task factory. Both branches produce these.
struct StorerCapable {
    topology: TopologyHandle<Arc<Identity>>,
    client_service: vertex_swarm_node::ClientService,
    client_handle: vertex_swarm_node::ClientHandle,
    run: RunTaskFn,
}

/// Assemble a bare `ClientNode` and its run-task factory.
async fn assemble_client_node(
    identity: &Arc<Identity>,
    network: &NetworkConfig<KademliaConfig>,
    node_store: Arc<dyn vertex_swarm_api::SwarmLocalStore>,
    peer_store: Option<PeerStore>,
    pseudosettle_event_sender: tokio::sync::mpsc::UnboundedSender<
        vertex_swarm_node::PseudosettleEvent,
    >,
    #[cfg(feature = "swap")] swap_event_sender: Option<
        tokio::sync::mpsc::UnboundedSender<vertex_swarm_node::SwapEvent>,
    >,
) -> Result<StorerCapable, SwarmNodeError> {
    let node_builder = ClientNode::builder(identity.clone())
        .with_store(node_store)
        .with_pseudosettle_events(pseudosettle_event_sender);
    #[cfg(feature = "swap")]
    let node_builder = match swap_event_sender {
        Some(tx) => node_builder.with_swap_events(tx),
        None => node_builder,
    };
    let (mut node, client_service, client_handle) = node_builder
        .build(network, peer_store)
        .await
        .map_err(|e| SwarmNodeError::Build(e.into()))?;
    let topology = node.topology_handle().clone();
    let forward_topology = topology.clone();

    let run: RunTaskFn = Box::new(move |accounting, _reporter, client_handle| {
        node.enable_forwarding(
            Arc::new(forward_topology),
            Arc::clone(&accounting),
            client_handle,
        );
        single_task(move |shutdown| async move {
            let _accounting = accounting;
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "Client node error");
            }
        })
    });

    Ok(StorerCapable {
        topology,
        client_service,
        client_handle,
        run,
    })
}

/// Assemble a `StorerNode` with the reserve-backed pullsync syncer, spawn its
/// puller over the topology seams, and return the run-task factory.
#[allow(clippy::too_many_arguments)]
async fn assemble_storer_node(
    ctx: &dyn InfrastructureContext,
    identity: &Arc<Identity>,
    network: &NetworkConfig<KademliaConfig>,
    node_store: Arc<dyn vertex_swarm_api::SwarmLocalStore>,
    peer_store: Option<PeerStore>,
    db: Option<Arc<RedbDatabase>>,
    reserve: Option<Arc<dyn vertex_swarm_api::BinCursorStore>>,
    pullsync: Option<Arc<dyn vertex_swarm_api::PullStorage>>,
    batches: Option<DbBatchStore<RedbDatabase>>,
    pseudosettle_event_sender: tokio::sync::mpsc::UnboundedSender<
        vertex_swarm_node::PseudosettleEvent,
    >,
    #[cfg(feature = "swap")] swap_event_sender: Option<
        tokio::sync::mpsc::UnboundedSender<vertex_swarm_node::SwapEvent>,
    >,
) -> Result<StorerCapable, SwarmNodeError> {
    use vertex_swarm_node::StorerNode;

    // A storer without a `PullStorage`-shaped reserve cannot serve pullsync; this
    // only happens with a reserve seam that is not the default `DbReserve`.
    let pullsync_storage =
        pullsync.ok_or_else(|| SwarmNodeError::Build(STORER_PULLSYNC_MISSING.into()))?;

    let node_builder = StorerNode::builder(identity.clone())
        .with_store(node_store)
        .with_pullsync_storage(pullsync_storage)
        .with_pseudosettle_events(pseudosettle_event_sender);
    #[cfg(feature = "swap")]
    let node_builder = match swap_event_sender {
        Some(tx) => node_builder.with_swap_events(tx),
        None => node_builder,
    };
    let (mut node, client_service, client_handle, pullsync_control) = node_builder
        .build(network, peer_store)
        .await
        .map_err(|e| SwarmNodeError::Build(e.into()))?;
    let topology = node.topology_handle().clone();

    // The reserve is the puller's admit seam (a `SwarmLocalStore` put) and the
    // pushsync ingest store; the interval store persists per-peer sync progress.
    let reserve = reserve.ok_or_else(|| SwarmNodeError::Build(STORER_RESERVE_MISSING.into()))?;
    let intervals = open_interval_store(db)?;
    // The puller consumes the node's pullsync control (its commands reach the run
    // loop and dispatch to the pullsync sub-behaviour); the node forwards
    // delivered pullsync events back through the returned handle.
    let puller_handle = spawn_storer_puller(
        ctx,
        topology.clone(),
        reserve.clone(),
        intervals,
        pullsync_control,
        batches,
    );
    node.set_puller(puller_handle);
    let forward_topology = topology.clone();

    let run: RunTaskFn = Box::new(move |accounting, _reporter, client_handle| {
        node.enable_forwarding(
            Arc::new(forward_topology),
            Arc::clone(&accounting),
            client_handle,
        );
        node.enable_storage(reserve as Arc<dyn vertex_swarm_api::ReserveStore>);
        single_task(move |shutdown| async move {
            let _accounting = accounting;
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "Storer node error");
            }
        })
    });

    Ok(StorerCapable {
        topology,
        client_service,
        client_handle,
        run,
    })
}

/// Guard message: a storer whose reserve is not the default `DbReserve` cannot
/// serve pullsync.
const STORER_PULLSYNC_MISSING: &str = "storer pullsync reserve view missing";

/// Open the puller's interval store over the shared database, or an in-memory
/// database when persistence is off (intervals reset on restart, matching the
/// in-memory reserve).
fn open_interval_store(
    db: Option<Arc<RedbDatabase>>,
) -> Result<Arc<vertex_swarm_storer::DbIntervalStore<RedbDatabase>>, SwarmNodeError> {
    let db = match db {
        Some(db) => db,
        None => RedbDatabase::in_memory()
            .map_err(|e| SwarmNodeError::Build(e.into()))?
            .into_arc(),
    };
    vertex_swarm_storer::DbIntervalStore::new(db)
        .map(Arc::new)
        .map_err(|e| SwarmNodeError::Build(e.into()))
}

/// Spawn the neighbourhood puller, returning the handle the node forwards
/// pullsync events through. The control surface lives on the node side and is
/// driven by the puller's `PullsyncControl` command channel.
fn spawn_storer_puller(
    ctx: &dyn InfrastructureContext,
    topology: TopologyHandle<Arc<Identity>>,
    reserve: Arc<dyn vertex_swarm_api::BinCursorStore>,
    intervals: Arc<vertex_swarm_storer::DbIntervalStore<RedbDatabase>>,
    control: vertex_swarm_node::StorerPullsyncControl,
    batches: Option<DbBatchStore<RedbDatabase>>,
) -> vertex_swarm_puller::PullerHandle {
    use vertex_swarm_api::PullChunkVerifier;
    use vertex_swarm_postage::AdmissionValidator;
    use vertex_swarm_puller::{
        FundingVerifier, PullerConfig, PullerSeams, SignatureVerifier, spawn_puller,
    };

    // With the default reserve the puller verifies on-chain batch funding against
    // the same batch set the reserve admits against; a reserve seam leaves no batch
    // handle, so the puller falls back to the signature-only gate.
    let verifier: Box<dyn PullChunkVerifier> = match batches {
        Some(batches) => Box::new(FundingVerifier::new(
            batches,
            AdmissionValidator::new(RESERVE_CONFIRMATION_THRESHOLD),
        )),
        None => Box::new(SignatureVerifier),
    };

    // The peer manager is the reporting authority: a neighbour that serves an
    // unverifiable chunk is scored down through it, the same path accounting and
    // the protocol handlers use.
    let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

    let seams = PullerSeams {
        control,
        intervals,
        verifier,
        // The reserve admits through its `SwarmLocalStore` put (the blanket
        // `ReserveAdmit` impl).
        admit: reserve as Arc<dyn vertex_swarm_api::SwarmLocalStore>,
        readiness: crate::pullsync::TopologyReadiness::new(topology.clone()),
        neighbours: crate::pullsync::TopologyNeighbours::new(topology),
        reporter,
    };
    spawn_puller(ctx.executor(), seams, PullerConfig::default())
}

/// Wrap a run task so the chain provider stays alive for the node's lifetime.
#[cfg(feature = "chain")]
fn wrap_with_chain(task: NodeTaskFn, chain_provider: Option<SharedChainProvider>) -> NodeTaskFn {
    Box::new(move |shutdown| {
        Box::pin(async move {
            let _chain_provider = chain_provider;
            task(shutdown).await;
        })
    })
}

impl SwarmLaunchConfig for BootnodeConfig {
    type Types = BootnodeLaunchTypes;
    type Providers = BootnodeComponents<TopologyHandle<Arc<Identity>>>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start(SwarmNodeType::Bootnode, self.spec());

        let db = open_shared_database(ctx);
        let peer_store = create_peer_store(&db);

        let node = BootNode::builder(self.identity().clone())
            .build(self.network(), peer_store)
            .await
            .map_err(|e| SwarmNodeError::Build(e.into()))?;

        let topology = node.topology_handle().clone();
        spawn_peer_manager_task(
            Arc::clone(topology.peer_manager()),
            DEFAULT_TICK_INTERVAL,
            ctx.executor(),
        );
        let providers = construct::bootnode(topology);

        let task = single_task(move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "BootNode error");
            }
        });

        info!("Bootnode built successfully");
        Ok((task, providers))
    }
}

impl SwarmLaunchConfig for ClientConfig {
    type Types = ClientLaunchTypes;
    type Providers = ClientComponents<TopologyHandle<Arc<Identity>>, VerifiedChunkProvider>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        build_client(self, ctx, None).await
    }
}

/// Build a client node. `cache == None` builds the default in-memory cache, no
/// reserve, so every pushsync relays and the opened database handle is ignored.
pub(crate) async fn build_client(
    config: ClientConfig,
    ctx: &dyn InfrastructureContext,
    cache: Option<CacheSeam>,
) -> Result<
    (
        NodeTaskFn,
        ClientComponents<TopologyHandle<Arc<Identity>>, VerifiedChunkProvider>,
    ),
    SwarmNodeError,
> {
    let cache_budget = config.local_store().cache_budget_bytes();
    let soc_ttl = config.local_store().soc_cache_ttl();
    let parts = build_client_backed_node(
        ctx,
        ClientNodeParams {
            node_type: SwarmNodeType::Client,
            spec: config.spec(),
            identity: config.identity(),
            network: config.network(),
            bandwidth: config.bandwidth(),
            verify: config.verify(),
            make_store: client_store_factory(cache, cache_budget, soc_ttl),
            #[cfg(feature = "chain")]
            chain: config.chain(),
            #[cfg(feature = "swap")]
            swap: config.swap(),
        },
    )
    .await?;

    let providers = construct::client(parts.topology, parts.chunks);
    Ok((parts.task, providers))
}

/// Resolve a client cache seam into the internal store factory. A client never
/// has a reserve, so the reserve view is always `None`.
fn client_store_factory(
    cache: Option<CacheSeam>,
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
) -> StoreFactory<'static> {
    match cache {
        None => Box::new(move |_db| {
            Ok(NodeStore {
                local: default_cache(cache_budget_bytes, soc_cache_ttl),
                reserve: None,
                pullsync: None,
                batches: None,
            })
        }),
        Some(CacheSeam::Ready(local)) => Box::new(move |_db| {
            Ok(NodeStore {
                local,
                reserve: None,
                pullsync: None,
                batches: None,
            })
        }),
        Some(CacheSeam::Factory(factory)) => Box::new(move |db| {
            Ok(NodeStore {
                local: factory(db)?,
                reserve: None,
                pullsync: None,
                batches: None,
            })
        }),
    }
}

/// The default client cache: a byte-bounded in-memory LRU sized from the config.
fn default_cache(
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
) -> Arc<dyn vertex_swarm_api::SwarmLocalStore> {
    Arc::new(vertex_swarm_localstore::ChunkStore::with_budget(
        cache_budget_bytes as usize,
        soc_cache_ttl,
    ))
}

impl SwarmLaunchConfig for StorerConfig {
    type Types = StorerLaunchTypes;
    type Providers = StorerComponents<
        TopologyHandle<Arc<Identity>>,
        VerifiedChunkProvider,
        Arc<dyn vertex_swarm_api::SwarmLocalStore>,
        Arc<dyn vertex_swarm_api::BinCursorStore>,
    >;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        build_storer(self, ctx, None, None).await
    }
}

/// Shared return type for the seam-aware entrypoint and the trait impl.
type StorerProviders = StorerComponents<
    TopologyHandle<Arc<Identity>>,
    VerifiedChunkProvider,
    Arc<dyn vertex_swarm_api::SwarmLocalStore>,
    Arc<dyn vertex_swarm_api::BinCursorStore>,
>;

/// Build a storer node, optionally overriding the cache and reserve through
/// builder seams.
///
/// Both `None` reproduces the default: the admission-gated [`DbReserve`] is the
/// pushsync-ingest reserve, layered under a default in-memory forwarding cache for
/// the retrieval-serve view. A reserve seam replaces the reserve; a cache seam
/// replaces the forwarding cache.
pub(crate) async fn build_storer(
    config: StorerConfig,
    ctx: &dyn InfrastructureContext,
    cache: Option<CacheSeam>,
    reserve: Option<ReserveSeam>,
) -> Result<(NodeTaskFn, StorerProviders), SwarmNodeError> {
    // Reserve capacity is a consensus quantity read from the spec, not local
    // disk: a fixed power-of-two chunk count from which the redistribution game
    // derives storage radius and committed depth, so nodes covering one
    // neighbourhood must agree on it regardless of disk.
    let _redistribution_enabled = config.storage().redistribution_enabled();
    let capacity = config.spec().reserve_capacity;
    let identity = config.identity().clone();
    let cache_budget = config.local_store().cache_budget_bytes();
    let soc_ttl = config.local_store().soc_cache_ttl();

    let parts = build_client_backed_node(
        ctx,
        ClientNodeParams {
            node_type: SwarmNodeType::Storer,
            spec: config.spec(),
            identity: config.identity(),
            network: config.network(),
            bandwidth: config.bandwidth(),
            verify: config.verify(),
            make_store: storer_store_factory(
                cache,
                reserve,
                identity,
                capacity,
                cache_budget,
                soc_ttl,
            ),
            #[cfg(feature = "chain")]
            chain: config.chain(),
            #[cfg(feature = "swap")]
            swap: config.swap(),
        },
    )
    .await?;

    // A missing reserve is a wiring bug: the launch path builds the default.
    let reserve = parts
        .reserve
        .ok_or_else(|| SwarmNodeError::Build(STORER_RESERVE_MISSING.into()))?;
    let providers = construct::storer(parts.topology, parts.chunks, parts.store, reserve);
    Ok((parts.task, providers))
}

/// The storer reserve resolved from its seam: the reserve view, the optional
/// pullsync server snapshot, and the optional second batch-store handle (both
/// present only for the default [`DbReserve`]).
type ResolvedReserve = (
    Arc<dyn vertex_swarm_api::BinCursorStore>,
    Option<Arc<dyn vertex_swarm_api::PullStorage>>,
    Option<DbBatchStore<RedbDatabase>>,
);

/// Resolve a storer's cache and reserve seams into the internal store factory.
///
/// The reserve (pushsync ingest) is carried separately while the retrieval-serve
/// view is a [`CacheThenReserve`] over both backends (reserve wins on overlap).
/// Defaults: the admission-gated [`DbReserve`] and an in-memory
/// [`vertex_swarm_localstore::ChunkStore`] cache sized from the local-store config.
fn storer_store_factory(
    cache: Option<CacheSeam>,
    reserve: Option<ReserveSeam>,
    identity: Arc<Identity>,
    capacity: u64,
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
) -> StoreFactory<'static> {
    Box::new(move |db| {
        // The pullsync server snapshot is only available for the default
        // `DbReserve`; a reserve seam erases to `BinCursorStore`, leaving pullsync
        // inbound serving unwired for that override.
        let (reserve, pullsync, batches): ResolvedReserve = match reserve {
            None => {
                let built = build_storer_reserve(db.clone(), &identity, capacity)?;
                (
                    built
                        .reserve
                        .ok_or_else(|| SwarmNodeError::Build(STORER_RESERVE_MISSING.into()))?,
                    built.pullsync,
                    built.batches,
                )
            }
            Some(ReserveSeam::Ready(reserve)) => (reserve, None, None),
            Some(ReserveSeam::Factory(factory)) => (factory(db.clone())?, None, None),
        };
        let cache: Arc<dyn vertex_swarm_api::SwarmLocalStore> = match cache {
            None => default_cache(cache_budget_bytes, soc_cache_ttl),
            Some(CacheSeam::Ready(cache)) => cache,
            Some(CacheSeam::Factory(factory)) => factory(db)?,
        };
        // The reserve upcasts to the local-store read side; writes land in the cache.
        let local: Arc<dyn vertex_swarm_api::SwarmLocalStore> =
            Arc::new(crate::composite::CacheThenReserve::new(
                cache,
                Arc::clone(&reserve) as Arc<dyn vertex_swarm_api::SwarmLocalStore>,
            ));
        Ok(NodeStore {
            local,
            reserve: Some(reserve),
            pullsync,
            batches,
        })
    })
}

/// Block confirmations a batch must accrue before the reserve admits chunks
/// stamped under it, so a reorg cannot retroactively invalidate admitted chunks.
const RESERVE_CONFIRMATION_THRESHOLD: u64 = 10;

/// Build the storer reserve over the shared database, erased to the local-store
/// trait.
///
/// Reuses the opened database when present so the reserve, its batch store and
/// the peer store share one handle; falls back to in-memory redb without
/// persistence. Open and table-creation failures surface as a build error.
///
/// Admits only stamped chunks, gated by a `DbBatchStore` (the batch set) and an
/// `AdmissionValidator` enforcing [`RESERVE_CONFIRMATION_THRESHOLD`] confirmations
/// plus structural and signature checks. The batch store starts empty, so the
/// reserve admits nothing until the postage indexer populates it.
fn build_storer_reserve(
    db: Option<Arc<RedbDatabase>>,
    identity: &Arc<Identity>,
    capacity: u64,
) -> Result<NodeStore, SwarmNodeError> {
    use vertex_swarm_api::StorageRadius;
    use vertex_swarm_postage::AdmissionValidator;
    use vertex_swarm_storer::EvictionStrategy;

    let db = match db {
        Some(db) => db,
        None => {
            warn!("Storer reserve running in-memory; stored chunks are lost on shutdown");
            RedbDatabase::in_memory()
                .map_err(|e| SwarmNodeError::Build(e.into()))?
                .into_arc()
        }
    };
    // Batch store and reserve share one handle so the postage ingest seam and the
    // reserve see a single consistent view. The clone is a second handle onto the
    // same tables, carried out for the puller's funding verifier.
    let batches =
        DbBatchStore::new(Arc::clone(&db)).map_err(|e| SwarmNodeError::Build(e.into()))?;
    let admission = AdmissionValidator::new(RESERVE_CONFIRMATION_THRESHOLD);
    let reserve = Arc::new(
        DbReserve::new(
            db,
            identity.as_ref(),
            batches.clone(),
            admission,
            capacity,
            EvictionStrategy::EvictFurthest,
            StorageRadius::ZERO,
        )
        .map_err(|e| SwarmNodeError::Build(e.into()))?,
    );
    // One `DbReserve`, three trait-object views: local-store (node and
    // components), reserve (pushsync ingest plus the served reserve capabilities),
    // and pullsync server snapshot (the inbound syncer's cursor and range source).
    Ok(NodeStore {
        local: Arc::clone(&reserve) as Arc<dyn vertex_swarm_api::SwarmLocalStore>,
        pullsync: Some(Arc::clone(&reserve) as Arc<dyn vertex_swarm_api::PullStorage>),
        reserve: Some(reserve as Arc<dyn vertex_swarm_api::BinCursorStore>),
        batches: Some(batches),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::{Path, PathBuf};

    use nectar_primitives::Nonce;
    use vertex_swarm_accounting::AccountingBuilder;
    use vertex_swarm_api::{Au, SwarmAccountingConfig, SwarmIdentity};
    use vertex_swarm_node::args::NetworkArgs;
    use vertex_swarm_peer_manager::{PeerManager, PeerManagerConfig};
    use vertex_swarm_spec::init_dev;
    use vertex_swarm_test_utils::{test_identity_arc, test_swarm_peer};
    use vertex_tasks::{TaskExecutor, TaskManager};

    /// Minimal infrastructure context for exercising the storage-mode flip.
    struct TestContext {
        executor: TaskExecutor,
        data_dir: PathBuf,
        db_path: Option<PathBuf>,
    }

    impl InfrastructureContext for TestContext {
        fn executor(&self) -> &TaskExecutor {
            &self.executor
        }

        fn data_dir(&self) -> &Path {
            &self.data_dir
        }

        fn db_path(&self) -> Option<&Path> {
            self.db_path.as_deref()
        }
    }

    /// A network config suitable for tests: OS-assigned port, no mDNS, no
    /// discovery, so nothing leaves the process.
    fn test_network_config() -> NetworkConfig<KademliaConfig> {
        let args = NetworkArgs {
            port: 0,
            mdns: false,
            disable_discovery: true,
            ..Default::default()
        };
        NetworkConfig::try_from(&args).expect("test network args are valid")
    }

    /// Without a configured database path the launch path must not open a
    /// database, and no consumer may fall back to a hardcoded location.
    #[tokio::test]
    async fn no_db_path_means_no_database_and_no_files() {
        let manager = TaskManager::current();
        let data_dir = tempfile::tempdir().expect("create tempdir");
        let ctx = TestContext {
            executor: manager.executor(),
            data_dir: data_dir.path().to_path_buf(),
            db_path: None,
        };

        let db = open_shared_database(&ctx);
        assert!(db.is_none(), "no db path must mean no database");
        assert!(
            create_peer_store(&db).is_none(),
            "peer snapshot store must be skipped without a database"
        );
        assert!(
            std::fs::read_dir(data_dir.path())
                .expect("read data dir")
                .next()
                .is_none(),
            "in-memory mode must not create files under the data dir"
        );
    }

    /// A configured database path is honored exactly: parent directories are
    /// created and the database file appears at the configured location.
    #[tokio::test]
    async fn db_path_opens_database_at_configured_location() {
        let manager = TaskManager::current();
        let data_dir = tempfile::tempdir().expect("create tempdir");
        let db_path = data_dir.path().join("custom").join("vertex.redb");
        let ctx = TestContext {
            executor: manager.executor(),
            data_dir: data_dir.path().to_path_buf(),
            db_path: Some(db_path.clone()),
        };

        let db = open_shared_database(&ctx);
        assert!(db.is_some(), "configured path must open a database");
        assert!(db_path.is_file(), "database file must exist at the path");
        assert!(
            create_peer_store(&db).is_some(),
            "peer snapshot store must back onto the opened database"
        );
    }

    /// An open failure on a configured path degrades to in-memory operation
    /// instead of failing the node build.
    #[tokio::test]
    async fn db_open_failure_degrades_to_in_memory() {
        let manager = TaskManager::current();
        let data_dir = tempfile::tempdir().expect("create tempdir");
        // The configured path nests under a regular file, so creating the
        // parent directory must fail.
        let blocker = data_dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").expect("write blocker file");
        let ctx = TestContext {
            executor: manager.executor(),
            data_dir: data_dir.path().to_path_buf(),
            db_path: Some(blocker.join("vertex.redb")),
        };

        let db = open_shared_database(&ctx);
        assert!(db.is_none(), "open failure must degrade to in-memory");
        assert!(create_peer_store(&db).is_none());
    }

    /// Building a full bootnode with `db_path() == None` leaves the data dir
    /// untouched: the node is fully in-memory.
    #[tokio::test]
    async fn bootnode_build_without_db_path_creates_no_files() {
        let manager = TaskManager::current();
        let data_dir = tempfile::tempdir().expect("create tempdir");
        let ctx = TestContext {
            executor: manager.executor(),
            data_dir: data_dir.path().to_path_buf(),
            db_path: None,
        };

        let spec = init_dev();
        // Bootnodes reject ephemeral identities, so build one through the
        // persistent constructor.
        let identity = Arc::new(Identity::new(
            alloy_signer_local::PrivateKeySigner::random(),
            Nonce::random(),
            spec.clone(),
            vertex_swarm_api::SwarmNodeType::Bootnode,
        ));
        let config = crate::config::BootnodeConfig::new(spec, identity, test_network_config());

        let (_task, _providers) = config.build(&ctx).await.expect("bootnode build succeeds");

        assert!(
            std::fs::read_dir(data_dir.path())
                .expect("read data dir")
                .next()
                .is_none(),
            "in-memory bootnode build must not create files under the data dir"
        );
    }

    /// An accounting violation must reach the peer manager wired as reporter and
    /// lower the peer's score.
    #[test]
    fn accounting_violation_reaches_peer_manager() {
        let identity = test_identity_arc();
        let peer_manager = PeerManager::new(&identity, PeerManagerConfig::default());
        let overlay = peer_manager.store_discovered_peer(test_swarm_peer(0xab));
        let baseline = peer_manager
            .get_peer_score(&overlay)
            .expect("stored peer has a score");

        let reporter: Arc<dyn PeerReporter> = peer_manager.clone();
        let config = DefaultBandwidthConfig::default();
        let over_limit = config.disconnect_threshold() + Au::new(1);
        let accounting = AccountingBuilder::new(config)
            .with_pricer_from_config(identity.spec().clone())
            .with_reporter(reporter)
            .build(&identity);

        // A debit projected past the disconnect threshold is the violation the
        // accounting reports.
        let result = accounting
            .bandwidth()
            .prepare_receive(overlay, over_limit, true);
        assert!(result.is_err());

        let after = peer_manager
            .get_peer_score(&overlay)
            .expect("peer still scored");
        assert!(after < baseline, "violation must lower the score");
    }

    /// The shared accounting wired into the client service via `with_accounting`
    /// debits the serving peer for an own-request delivery. This proves the
    /// builder's wiring expression activates the origin debit, not just that the
    /// service supports it.
    #[tokio::test]
    async fn builder_wiring_debits_an_origin_delivery() {
        use nectar_primitives::{AnyChunk, ChunkAddress, ContentChunk};
        use vertex_swarm_api::{SwarmBandwidthAccounting, SwarmPeerBandwidth, SwarmPricing};
        use vertex_swarm_node::{ClientEvent, ClientService};

        let identity = test_identity_arc();
        let config = DefaultBandwidthConfig::default();
        let accounting = Arc::new(
            AccountingBuilder::new(config)
                .with_pricer_from_config(identity.spec().clone())
                .build(&identity),
        );

        let chunk: AnyChunk = ContentChunk::new(b"origin debit through the builder".to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        let overlay = ChunkAddress::from([0x5cu8; 32]);
        let price = accounting.pricing().peer_price(&overlay, &address);
        assert!(price > Au::ZERO, "the per-chunk price is non-zero");

        // The same wiring expression `build_client_backed_node` uses.
        let (service, event_tx, _handle) = ClientService::new();
        let service = service.with_accounting(
            Arc::new(accounting.pricing().clone()),
            accounting.bandwidth().clone(),
        );

        let manager = TaskManager::current();
        let handle = manager
            .executor()
            .spawn_service("test.client_service", service);

        event_tx
            .send(ClientEvent::ChunkReceived {
                peer: overlay,
                address,
                chunk,
                stamp: None,
                latency: std::time::Duration::from_millis(1),
                originated: true,
            })
            .await
            .expect("service is running");
        // Dropping the sender closes the channel, so the run loop drains the one
        // queued event and then exits.
        drop(event_tx);
        handle.await.expect("service task joins cleanly");

        assert_eq!(
            accounting.bandwidth().for_peer(overlay).balance(),
            -price,
            "an own-request delivery debits the serving peer by the per-chunk price"
        );
    }

    /// The storer factory builds the admission-gated reserve, not the cache-only
    /// client store: a put for an unknown batch is rejected, proving admission is
    /// wired. The full admissible-put path is covered by the reserve crate.
    #[test]
    fn storer_reserve_factory_builds_admission_gated_store() {
        use alloy_primitives::{B256, Signature};
        use nectar_postage::Stamp;
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let identity = test_identity_arc();
        // A small power of two matches the consensus chunk-count shape cheaply.
        let capacity: u64 = 1 << 12;

        // db = None exercises the in-memory fallback.
        let node_store = build_storer_reserve(None, &identity, capacity).expect("reserve builds");
        assert!(
            node_store.reserve.is_some(),
            "the storer factory yields the reserve view"
        );
        let store = node_store.local;

        // A fresh storer reserve knows no batches, so serving a chunk it never
        // held returns nothing rather than erroring.
        let absent_chunk: AnyChunk = ContentChunk::new(b"never stored".to_vec())
            .expect("valid content chunk")
            .into();
        let absent = *absent_chunk.address();
        assert!(
            store
                .get(&absent)
                .expect("reserve get does not error")
                .is_none(),
            "an empty reserve serves nothing"
        );

        // A stamped put for a batch unknown to the empty batch store is rejected
        // by the wired admission validator, proving the factory did not fall back
        // to a store that accepts arbitrary chunks.
        let chunk: AnyChunk = ContentChunk::new(b"unadmissible".to_vec())
            .expect("valid content chunk")
            .into();
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let cached = CachedChunk::new(chunk, Some(stamp));
        let address = *cached.address();

        let err = store
            .put(cached)
            .expect_err("a stamp for an unknown batch must be refused by admission");
        assert!(
            matches!(err, vertex_swarm_api::SwarmError::InvalidChunk { .. }),
            "admission rejects the put as an invalid chunk, got {err:?}"
        );
        assert!(
            !store.contains(&address),
            "a rejected put leaves nothing in the reserve"
        );
    }

    #[test]
    fn client_factory_default_builds_a_working_cache() {
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let factory = client_store_factory(None, 1 << 20, DEFAULT_SOC_CACHE_TTL_NS_TEST);
        let node_store = factory(None).expect("default cache builds");
        assert!(
            node_store.reserve.is_none(),
            "a client never carries a reserve"
        );

        let chunk: AnyChunk = ContentChunk::new(b"cached content".to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        node_store
            .local
            .put(CachedChunk::new(chunk, None))
            .expect("the default cache accepts a content chunk");
        assert!(
            node_store.local.contains(&address),
            "the default cache serves what it stored"
        );
    }

    #[test]
    fn client_factory_honors_a_ready_cache_seam() {
        let cache: Arc<dyn vertex_swarm_api::SwarmLocalStore> = Arc::new(
            vertex_swarm_localstore::ChunkStore::with_budget(4096, DEFAULT_SOC_CACHE_TTL_NS_TEST),
        );
        let factory = client_store_factory(Some(CacheSeam::Ready(Arc::clone(&cache))), 0, 0);
        let node_store = factory(None).expect("seam cache is used");
        assert!(
            Arc::ptr_eq(&cache, &node_store.local),
            "the supplied cache must reach the node store unchanged"
        );
        assert!(node_store.reserve.is_none());
    }

    #[test]
    fn storer_factory_honors_a_ready_reserve_seam() {
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let identity = test_identity_arc();
        let seam_reserve = build_storer_reserve(None, &identity, 1 << 12)
            .expect("reserve builds")
            .reserve
            .expect("reserve view present");

        let factory = storer_store_factory(
            None,
            Some(ReserveSeam::Ready(Arc::clone(&seam_reserve))),
            identity,
            1 << 12,
            1 << 20,
            DEFAULT_SOC_CACHE_TTL_NS_TEST,
        );
        let node_store = factory(None).expect("seam reserve is used");
        let reserve = node_store.reserve.expect("storer always has a reserve");
        assert!(
            Arc::ptr_eq(&seam_reserve, &reserve),
            "the supplied reserve must reach the node store as the ingest view"
        );

        // A put through the serve view lands in the cache, never the reserve.
        let chunk: AnyChunk = ContentChunk::new(b"forwarded out-of-aor".to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        node_store
            .local
            .put(CachedChunk::new(chunk, None))
            .expect("the forwarding cache accepts a content chunk");
        assert!(
            node_store.local.contains(&address),
            "the retrieval-serve view serves the cached chunk"
        );
        assert!(
            !reserve.contains(&address),
            "a put through the serve view must not reach the reserve"
        );
    }

    /// The default storer path (`None`, `None`) layers the default cache over the
    /// built reserve, with serve-view writes reaching the cache only.
    #[test]
    fn storer_factory_default_layers_cache_over_built_reserve() {
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let identity = test_identity_arc();
        let factory = storer_store_factory(
            None,
            None,
            identity,
            1 << 12,
            1 << 20,
            DEFAULT_SOC_CACHE_TTL_NS_TEST,
        );
        let node_store = factory(None).expect("default storer store builds");
        let reserve = node_store
            .reserve
            .expect("the default storer factory yields a reserve view");

        // A put through the serve view lands in the default cache, never the reserve.
        let chunk: AnyChunk = ContentChunk::new(b"forwarded out-of-aor default".to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        node_store
            .local
            .put(CachedChunk::new(chunk, None))
            .expect("the default forwarding cache accepts a content chunk");
        assert!(
            node_store.local.contains(&address),
            "the retrieval-serve view serves the cached chunk"
        );
        assert!(
            !reserve.contains(&address),
            "a put through the serve view must not reach the built reserve"
        );
    }

    /// Test SOC cache TTL: any non-zero value works for the cache-shape tests.
    const DEFAULT_SOC_CACHE_TTL_NS_TEST: u64 = vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS;

    /// A storer with no `--chain.rpc-url` hard-fails with [`SwarmNodeError::ChainRequired`]
    /// rather than degrade chainless.
    #[cfg(feature = "chain")]
    #[tokio::test]
    async fn storer_without_chain_config_errors_chain_required() {
        let spec = init_dev();
        let identity = test_identity_arc();
        let chain = ChainConfig::default();
        assert!(
            chain.rpc_url.is_none(),
            "default chain config has no RPC URL"
        );

        let err = build_node_chain_provider(
            &spec,
            &identity,
            SwarmNodeType::Storer,
            // A storer always needs the chain, so swap_enabled is irrelevant.
            false,
            &chain,
        )
        .await
        .expect_err("a storer without chain config must hard-fail");
        assert!(
            matches!(
                err,
                SwarmNodeError::ChainRequired {
                    node_type: SwarmNodeType::Storer
                }
            ),
            "a chainless storer must error with ChainRequired{{Storer}}, got {err:?}"
        );
    }

    /// A storer on a network with no canonical deployment hard-fails even with a
    /// valid `--chain.rpc-url`: there is no address book to target the contracts.
    #[cfg(feature = "chain")]
    #[tokio::test]
    async fn storer_on_deployment_less_network_errors_chain_required() {
        let spec = init_dev();
        let identity = test_identity_arc();
        // A valid RPC URL passes the rpc_url check and reaches the deployment lookup.
        let chain = ChainConfig {
            rpc_url: Some("https://rpc.example".to_string()),
            ..ChainConfig::default()
        };

        let err = build_node_chain_provider(&spec, &identity, SwarmNodeType::Storer, false, &chain)
            .await
            .expect_err("a storer on a deployment-less network must hard-fail");
        assert!(
            matches!(
                err,
                SwarmNodeError::ChainRequired {
                    node_type: SwarmNodeType::Storer
                }
            ),
            "a deployment-less storer must error with ChainRequired{{Storer}}, got {err:?}"
        );
    }

    /// A pure light client (no SWAP) does not need a chain, so the provider step
    /// degrades to `Ok(None)` even with no RPC URL configured.
    #[cfg(feature = "chain")]
    #[tokio::test]
    async fn light_client_builds_chainless() {
        let spec = init_dev();
        let identity = test_identity_arc();
        let chain = ChainConfig::default();

        let provider = build_node_chain_provider(
            &spec,
            &identity,
            SwarmNodeType::Client,
            // No SWAP: a pure light client stays chain-free.
            false,
            &chain,
        )
        .await
        .expect("a chain-free client must not require a chain");
        assert!(
            provider.is_none(),
            "a light client degrades chainless, building no provider"
        );
    }

    /// A SWAP-enabled client needs the chain to settle on-chain, so a missing
    /// RPC URL hard-fails the same way a storer does.
    #[cfg(feature = "chain")]
    #[tokio::test]
    async fn swap_client_without_chain_config_errors_chain_required() {
        let spec = init_dev();
        let identity = test_identity_arc();
        let chain = ChainConfig::default();

        let err = build_node_chain_provider(
            &spec,
            &identity,
            SwarmNodeType::Client,
            // SWAP enabled: the client now needs a chain to settle.
            true,
            &chain,
        )
        .await
        .expect_err("a SWAP-enabled client without chain config must hard-fail");
        assert!(
            matches!(
                err,
                SwarmNodeError::ChainRequired {
                    node_type: SwarmNodeType::Client
                }
            ),
            "a chainless SWAP client must error with ChainRequired{{Client}}, got {err:?}"
        );
    }
}
