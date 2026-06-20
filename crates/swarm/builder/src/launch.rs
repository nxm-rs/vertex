//! SwarmLaunchConfig implementations for config types.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use vertex_net_peer_store::PeerSnapshotStore;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
#[cfg(feature = "chain")]
use vertex_swarm_api::SwarmSpec;
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, PeerReporter, StorerComponents, SwarmAccountingConfig,
    SwarmClientAccounting, SwarmLaunchConfig, SwarmNodeType, construct,
};
use vertex_swarm_bandwidth::{
    Accounting, AccountingBuilder, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
};
use vertex_swarm_bandwidth_pseudosettle::PseudosettleProvider;
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_node::{AccountingSettlement, BootNode, ClientNode, PeerSelector, SelfThrottle};
use vertex_swarm_peer_manager::{
    DEFAULT_TICK_INTERVAL, DbPeerSnapshotStore, PeerSnapshot, spawn_peer_manager_task,
};
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

/// Build and validate the shared chain provider for a chain-needing node.
///
/// Returns `Ok(None)` when the chain is deliberately skipped (no RPC URL, or a
/// network with no canonical deployment), `Err` only when a configured
/// connection fails. The returned [`SharedChainProvider`] is a cloneable handle,
/// not a spawned service.
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
        warn!(
            "Chain required for this node but no --chain.rpc-url configured; chain access not enabled"
        );
        return Ok(None);
    };

    let Some(address_book) = ChainAddressBook::from_swarm(spec.swarm()) else {
        warn!("Chain has no canonical deployment for this network; chain access not enabled");
        return Ok(None);
    };

    let signer = (*identity.signer()).clone();
    let provider = crate::chain::build_chain_provider(rpc_url, signer, address_book).await?;

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

/// The node's local store, plus the storer reserve view when the node is a storer.
///
/// A storer's local store *is* its reserve. `ReserveStore: SwarmLocalStore`, so
/// the one `DbReserve` upcasts to the local-store view (trait upcasting is stable
/// at the workspace MSRV); both views are carried so the single instance is reused
/// without rebuilding: `local` for retrieval and components, `reserve` for
/// pushsync ingest. A client leaves `reserve` `None`.
struct NodeStore {
    local: Arc<dyn vertex_swarm_api::SwarmLocalStore>,
    reserve: Option<Arc<dyn vertex_swarm_api::ReserveStore>>,
}

/// A cache override supplied through the builder.
///
/// `Ready` hands the launch path a fully constructed cache; `Factory` defers
/// construction to build time and is given the opened shared database (if any),
/// so an embedder can size or back the cache from the same handle the rest of
/// the node uses. When no seam is supplied the launch path builds the default
/// in-memory [`vertex_swarm_localstore::ChunkStore`] sized from the local-store
/// config, leaving existing callers unchanged.
///
/// This crate is native-only (see the crate-root docs), so both variants are
/// always available.
pub(crate) enum CacheSeam {
    /// A pre-built cache, used as-is.
    Ready(Arc<dyn vertex_swarm_api::SwarmLocalStore>),
    /// A factory invoked at build time with the opened shared database.
    Factory(CacheFactory),
}

/// A reserve override supplied through the builder.
///
/// Mirrors [`CacheSeam`] for the storer reserve view. When no seam is supplied
/// the storer launch path builds the default admission-gated [`DbReserve`] over
/// the shared database.
pub(crate) enum ReserveSeam {
    /// A pre-built reserve, used as-is.
    Ready(Arc<dyn vertex_swarm_api::ReserveStore>),
    /// A factory invoked at build time with the opened shared database.
    Factory(ReserveFactory),
}

/// Builds a cache from the opened shared database (if any).
///
/// The database handle is `RedbDatabase`-typed; this crate is native-only (see
/// the crate-root docs), so the factory is always available.
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
        ) -> Result<Arc<dyn vertex_swarm_api::ReserveStore>, SwarmNodeError>
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
}

/// Wire the pseudosettle provider when the mode enables it. The caller adds swap
/// afterwards, so `Both` ends up pseudosettle-then-swap.
fn with_default_settlement<P>(
    builder: AccountingBuilder<DefaultBandwidthConfig, P>,
    bandwidth: &DefaultBandwidthConfig,
) -> AccountingBuilder<DefaultBandwidthConfig, P> {
    if SwarmAccountingConfig::mode(bandwidth).pseudosettle_enabled() {
        builder.with_settlement(PseudosettleProvider::new(bandwidth.clone()))
    } else {
        builder
    }
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

    let db = open_shared_database(ctx);
    let peer_store = create_peer_store(&db);

    let NodeStore {
        local: store,
        reserve,
    } = (params.make_store)(db.clone())?;
    let node_store = Arc::clone(&store);

    // SWAP settlement is prepared first: the provider embeds in the accounting
    // and the swap event sink routes at node build time.
    #[cfg(feature = "swap")]
    let (swap_provider, swap_wiring) = crate::swap::SwapWiring::prepare(
        params.spec,
        params.identity,
        params.bandwidth,
        params.swap,
    )
    .unzip();

    let node_builder = ClientNode::builder(params.identity.clone()).with_store(node_store);
    #[cfg(feature = "swap")]
    let node_builder = match swap_wiring.as_ref() {
        Some(wiring) => node_builder.with_swap_events(wiring.swap_event_sender()),
        None => node_builder,
    };
    let (mut node, client_service, client_handle) = node_builder
        .build(params.network, peer_store)
        .await
        .map_err(|e| SwarmNodeError::Build(e.into()))?;

    let topology = node.topology_handle().clone();
    spawn_peer_manager_task(
        Arc::clone(topology.peer_manager()),
        DEFAULT_TICK_INTERVAL,
        ctx.executor(),
    );

    // The peer manager is the reporting authority: accounting and the settlement
    // services report violations through it so misbehaving peers are scored down.
    let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

    let accounting_builder = AccountingBuilder::new(params.bandwidth.clone())
        .with_pricer_from_config(Arc::clone(params.spec))
        .with_reporter(Arc::clone(&reporter));

    // Pseudosettle before swap, so settlement tries time-based forgiveness first.
    let accounting_builder = with_default_settlement(accounting_builder, params.bandwidth);

    #[cfg(feature = "swap")]
    let accounting = match swap_provider {
        Some(provider) => accounting_builder
            .with_settlement(provider)
            .build(params.identity),
        None => accounting_builder.build(params.identity),
    };
    #[cfg(not(feature = "swap"))]
    let accounting = accounting_builder.build(params.identity);
    // One accounting instance is shared by the selector, the two-leg forwarder,
    // and the node task that keeps it alive.
    let accounting = Arc::new(accounting);

    // Multi-hop forwarding: a retrieval cache miss relays to a strictly-closer
    // peer and an inbound pushsync relays toward the chunk's neighbourhood,
    // accounting both legs over the same instance. Must precede the event loop.
    node.enable_forwarding(
        Arc::new(topology.clone()),
        Arc::clone(&accounting),
        client_handle.clone(),
    );

    // Storer ingest: the inbound pushsync path gets the reserve so a delivery the
    // node is responsible for is stored and acknowledged with a signed receipt
    // instead of relayed. A client has no reserve and keeps the verbatim relay.
    if let Some(reserve) = reserve {
        node.enable_storage(reserve);
    }

    let selector = Arc::new(PeerSelector::new(
        Arc::new(topology.clone()),
        accounting.bandwidth().clone(),
        Arc::new(accounting.pricing().clone()),
        Arc::new(AccountingSettlement::new(accounting.bandwidth().clone())),
    ));

    // Outbound self-throttle: pace our retrieval and pushsync requests under each
    // peer's pseudosettle allowance so a burst never crosses the settlement
    // trigger. See `SelfThrottle` for the token/price/sizing model.
    let throttle = Arc::new(SelfThrottle::new(&accounting, params.bandwidth));
    let throttled_handle = client_handle.clone().with_throttle(Arc::clone(&throttle));

    let chunk_provider =
        NetworkChunkProvider::new(throttled_handle, topology.clone()).with_selector(selector);
    let chunks = VerifyingChunkProvider::new(chunk_provider, params.verify);

    // The client service reports retrieval and pushsync outcomes through the same
    // peer manager authority accounting uses, and shares the handle's throttle so
    // a peer disconnect clears that peer's bucket.
    let client_service = client_service
        .with_reporter(Arc::clone(&reporter))
        .with_throttle(throttle);
    ctx.executor()
        .spawn_service("swarm.client_service", client_service);

    // A storer always needs a chain (staking, oracle, settlement); a client
    // needs one only when SWAP settlement is enabled.
    #[cfg(feature = "chain")]
    let chain_provider = {
        let swap_enabled = SwarmAccountingConfig::mode(params.bandwidth).swap_enabled();
        build_node_chain_provider(
            params.spec,
            params.identity,
            node_type,
            swap_enabled,
            params.chain,
        )
        .await?
    };

    // SWAP settlement service over the shared accounting: forwards cheque
    // commands to the node and cashes received cheques on chain when a provider
    // is present.
    #[cfg(feature = "swap")]
    if let Some(wiring) = swap_wiring {
        wiring.spawn(
            ctx,
            accounting.bandwidth().clone(),
            client_handle,
            Arc::clone(&reporter),
            #[cfg(feature = "chain")]
            chain_provider.as_ref(),
        );
    }

    // Accounting and the chain provider are moved into the task to keep them
    // alive for the node's lifetime.
    let task = single_task(move |shutdown| async move {
        let _accounting = accounting;
        #[cfg(feature = "chain")]
        let _chain_provider = chain_provider;
        if let Err(e) = node.start_and_run(shutdown).await {
            tracing::error!(error = %e, %node_type, "Node error");
        }
    });

    info!(%node_type, "Node built successfully");
    Ok(ClientNodeParts {
        task,
        topology,
        chunks,
        store,
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

/// Build a client node, optionally overriding the cache through a builder seam.
///
/// `cache == None` reproduces the default cache: a byte-bounded in-memory
/// [`vertex_swarm_localstore::ChunkStore`] sized from the local-store config, no
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

/// Resolve a client cache seam into the internal store factory.
///
/// With no seam the factory builds the default in-memory cache sized from the
/// config; a `Ready` seam returns the supplied cache verbatim; a `Factory` seam
/// is invoked at build time with the opened shared database. A client never has a
/// reserve, so the reserve view is always `None`.
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
            })
        }),
        Some(CacheSeam::Ready(local)) => Box::new(move |_db| {
            Ok(NodeStore {
                local,
                reserve: None,
            })
        }),
        Some(CacheSeam::Factory(factory)) => Box::new(move |db| {
            Ok(NodeStore {
                local: factory(db)?,
                reserve: None,
            })
        }),
    }
}

/// The default client cache: a byte-bounded in-memory LRU sized from the
/// local-store config, erased to the local-store trait.
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
    >;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        build_storer(self, ctx, None, None).await
    }
}

/// The storer providers, named so the seam-aware entrypoint and the trait impl
/// share one return type.
type StorerProviders = StorerComponents<
    TopologyHandle<Arc<Identity>>,
    VerifiedChunkProvider,
    Arc<dyn vertex_swarm_api::SwarmLocalStore>,
>;

/// Build a storer node, optionally overriding the cache and reserve through
/// builder seams.
///
/// Both `None` reproduces the default: the admission-gated [`DbReserve`] over the
/// shared database serves as both the reserve (pushsync ingest) and the local
/// store (retrieval and components). A reserve seam replaces that reserve; a
/// cache seam replaces only the local-store view, leaving the reserve view to the
/// supplied or default reserve.
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

    let parts = build_client_backed_node(
        ctx,
        ClientNodeParams {
            node_type: SwarmNodeType::Storer,
            spec: config.spec(),
            identity: config.identity(),
            network: config.network(),
            bandwidth: config.bandwidth(),
            verify: config.verify(),
            make_store: storer_store_factory(cache, reserve, identity, capacity),
            #[cfg(feature = "chain")]
            chain: config.chain(),
            #[cfg(feature = "swap")]
            swap: config.swap(),
        },
    )
    .await?;

    let providers = construct::storer(parts.topology, parts.chunks, parts.store);
    Ok((parts.task, providers))
}

/// Resolve a storer's cache and reserve seams into the internal store factory.
///
/// With no reserve seam the default admission-gated reserve is built over the
/// shared database. The resolved reserve is the local store unless a cache seam
/// supplies a separate local-store view.
fn storer_store_factory(
    cache: Option<CacheSeam>,
    reserve: Option<ReserveSeam>,
    identity: Arc<Identity>,
    capacity: u64,
) -> StoreFactory<'static> {
    Box::new(move |db| {
        // Resolve the reserve view, then the local-store view. With no cache
        // seam the reserve is the local store, so the default path reuses the
        // single `DbReserve` that `build_storer_reserve` erases to both views.
        let reserve: Arc<dyn vertex_swarm_api::ReserveStore> = match reserve {
            None => {
                let store = build_storer_reserve(db.clone(), &identity, capacity)?;
                // No cache seam: the reserve is already the local store, so return
                // the paired views as-is without splitting them back apart.
                if cache.is_none() {
                    return Ok(store);
                }
                store
                    .reserve
                    .ok_or_else(|| SwarmNodeError::Build("storer reserve missing".into()))?
            }
            Some(ReserveSeam::Ready(reserve)) => reserve,
            Some(ReserveSeam::Factory(factory)) => factory(db.clone())?,
        };
        // A cache seam overrides the local-store view; otherwise the reserve is
        // the local store.
        let local: Arc<dyn vertex_swarm_api::SwarmLocalStore> = match cache {
            None => Arc::clone(&reserve) as Arc<dyn vertex_swarm_api::SwarmLocalStore>,
            Some(CacheSeam::Ready(local)) => local,
            Some(CacheSeam::Factory(factory)) => factory(db)?,
        };
        Ok(NodeStore {
            local,
            reserve: Some(reserve),
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
    use vertex_swarm_postage::{AdmissionValidator, DbBatchStore};
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
    // reserve see a single consistent view.
    let batches =
        DbBatchStore::new(Arc::clone(&db)).map_err(|e| SwarmNodeError::Build(e.into()))?;
    let admission = AdmissionValidator::new(RESERVE_CONFIRMATION_THRESHOLD);
    let reserve = Arc::new(
        DbReserve::new(
            db,
            identity.as_ref(),
            batches,
            admission,
            capacity,
            EvictionStrategy::EvictFurthest,
            StorageRadius::ZERO,
        )
        .map_err(|e| SwarmNodeError::Build(e.into()))?,
    );
    // One `DbReserve`, two trait-object views: local-store (node and components)
    // and reserve (pushsync ingest).
    Ok(NodeStore {
        local: Arc::clone(&reserve) as Arc<dyn vertex_swarm_api::SwarmLocalStore>,
        reserve: Some(reserve as Arc<dyn vertex_swarm_api::ReserveStore>),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::{Path, PathBuf};

    use nectar_primitives::Nonce;
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

    /// Client and storer both get a pseudosettle provider; a `None` mode wires none.
    #[test]
    fn default_settlement_wires_pseudosettle_per_node_type() {
        let identity = test_identity_arc();

        let build = |node_type| {
            let bandwidth = DefaultBandwidthConfig::for_node_type(node_type);
            let builder = AccountingBuilder::new(bandwidth.clone())
                .with_pricer_from_config(identity.spec().clone());
            with_default_settlement(builder, &bandwidth)
                .build(&identity)
                .bandwidth()
                .provider_names()
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
        };

        // Both carry pseudosettle; swap is wired separately and not exercised here.
        assert_eq!(
            build(SwarmNodeType::Client),
            vec!["pseudosettle".to_string()]
        );
        assert_eq!(
            build(SwarmNodeType::Storer),
            vec!["pseudosettle".to_string()]
        );

        // A `None` mode wires no settlement provider at all.
        let none_bandwidth = DefaultBandwidthConfig::for_node_type(SwarmNodeType::Bootnode);
        assert_eq!(none_bandwidth.mode(), vertex_swarm_api::BandwidthMode::None);
        let none_builder = AccountingBuilder::new(none_bandwidth.clone())
            .with_pricer_from_config(identity.spec().clone());
        assert!(
            with_default_settlement(none_builder, &none_bandwidth)
                .build(&identity)
                .bandwidth()
                .provider_names()
                .is_empty(),
            "a None mode must wire no settlement provider"
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

    /// With no seam the client factory builds the default in-memory cache sized
    /// from the supplied budget, accepts a content chunk, and exposes no reserve.
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

    /// A `with_cache` seam is used verbatim: the same `Arc` reaches the node
    /// store, no default cache is built.
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

    /// A `with_reserve` seam is used as both the reserve and, with no cache seam,
    /// the local-store view.
    #[test]
    fn storer_factory_honors_a_ready_reserve_seam() {
        let identity = test_identity_arc();
        // Build a real reserve to use as the seam value.
        let seam_reserve = build_storer_reserve(None, &identity, 1 << 12)
            .expect("reserve builds")
            .reserve
            .expect("reserve view present");

        let factory = storer_store_factory(
            None,
            Some(ReserveSeam::Ready(Arc::clone(&seam_reserve))),
            identity,
            1 << 12,
        );
        let node_store = factory(None).expect("seam reserve is used");
        let reserve = node_store.reserve.expect("storer always has a reserve");
        assert!(
            Arc::ptr_eq(&seam_reserve, &reserve),
            "the supplied reserve must reach the node store unchanged"
        );
        // With no cache seam the reserve is the local store: both views point at
        // the one allocation, so the strong count counts reserve, local, and the
        // seam handle still held here.
        assert_eq!(
            Arc::strong_count(&reserve),
            3,
            "the reserve and the local-store view share one allocation"
        );
    }

    /// Test SOC cache TTL: any non-zero value works for the cache-shape tests.
    const DEFAULT_SOC_CACHE_TTL_NS_TEST: u64 = vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS;
}
