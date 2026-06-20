//! SwarmLaunchConfig implementations for config types.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use vertex_net_peer_store::PeerSnapshotStore;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, PeerReporter, StorerComponents, SwarmClientAccounting,
    SwarmLaunchConfig, SwarmNodeType, construct,
};
#[cfg(feature = "chain")]
use vertex_swarm_api::{SwarmAccountingConfig, SwarmSpec};
use vertex_swarm_bandwidth::{
    Accounting, AccountingBuilder, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
};
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
/// `ctx.db_path()` is the single source of truth for the storage mode: `None`
/// means the node runs fully in-memory and no database is opened, `Some(path)`
/// opens (or creates) the database file there and spawns the periodic metrics
/// task. An open failure on a configured path degrades to in-memory operation
/// instead of aborting the node; the warning spells out what is lost.
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

/// Construct, validate, and log the shared chain provider for a chain-needing
/// node.
///
/// Selects the network address book from the spec, then builds and validates a
/// wallet-filled alloy provider over the configured RPC URL signed by the node's
/// Ethereum identity. The chain is a shared provider, not a long-lived service,
/// so there is nothing to spawn: the returned [`SharedChainProvider`] is a
/// cloneable handle that future chain consumers (the SWAP settlement service in
/// a later PR) borrow to build their clients (for example a
/// `ChequebookContract`).
///
/// Returns:
/// - `Ok(Some(provider))` when the chain is connected and validated.
/// - `Ok(None)` when the chain is deliberately skipped (no RPC URL configured,
///   or a development network with no canonical deployment).
/// - `Err` only when a configured connection fails to connect or validate.
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
    /// Associated type set for the bootnode launch path: spec, identity, and
    /// topology only, with no bandwidth accounting.
    BootnodeLaunchTypes
);
define_launch_types!(
    /// Associated type set for the client launch path: the bootnode types plus
    /// the default bandwidth accounting stack.
    ClientLaunchTypes,
    with_client
);
define_launch_types!(
    /// Associated type set for the storer launch path: the bootnode types plus
    /// the default bandwidth accounting stack.
    StorerLaunchTypes,
    with_client
);

/// Factory that turns the opened shared database (if any) into the node's
/// `SwarmLocalStore`.
///
/// The store is the single surface the retrieval handler consults before going
/// to the network, so each node type wires its own: the client builds a
/// byte-bounded in-memory cache (the database handle is ignored), while the
/// storer builds the persisting reserve over the same database the peer store
/// backs onto. The factory receives the opened database so the storer reserve
/// and the peer store share one handle rather than opening the file twice.
type StoreFactory<'a> = Box<
    dyn FnOnce(
            Option<Arc<RedbDatabase>>,
        ) -> Result<Arc<dyn vertex_swarm_api::SwarmLocalStore>, SwarmNodeError>
        + Send
        + 'a,
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

/// Outputs of [`build_client_backed_node`]: the long-running node task plus the
/// handles the node-type-specific RPC providers wrap.
struct ClientNodeParts {
    task: NodeTaskFn,
    topology: TopologyHandle<Arc<Identity>>,
    chunks: VerifiedChunkProvider,
    /// The node's local store, erased to the trait. The node holds its own
    /// clone; the storer wires this one as its components' store so the
    /// retrieval-serving reserve and the served store are the same instance.
    store: Arc<dyn vertex_swarm_api::SwarmLocalStore>,
}

/// Shared launch path for the node types backed by a client node (client and
/// storer).
///
/// Builds the node, then the bandwidth accounting (reporting violations to the
/// peer manager, with SWAP settlement embedded when enabled) and the verified
/// chunk provider with score- and affordability-aware candidate selection,
/// spawns the client service and the SWAP settlement service, connects the
/// chain provider when the node type requires one, and wraps the node run loop
/// in a task that owns the accounting and chain handles for the node's
/// lifetime.
async fn build_client_backed_node(
    ctx: &dyn InfrastructureContext,
    params: ClientNodeParams<'_>,
) -> Result<ClientNodeParts, SwarmNodeError> {
    let node_type = params.node_type;
    log_build_start(node_type, params.spec);

    let db = open_shared_database(ctx);
    let peer_store = create_peer_store(&db);

    // Build the node's local store from the opened database. The client ignores
    // the handle and runs an in-memory cache; the storer builds the persisting
    // reserve over the same `db` so the reserve and the peer store share one
    // handle. The same store serves inbound retrievals and holds local
    // deliveries: the retrieval handler consults `SwarmLocalStore::get` before
    // the network, so a storer serves from its reserve.
    let store = (params.make_store)(db.clone())?;
    // The node consumes its own clone; the returned handle is the same instance
    // the storer surfaces as its components' store.
    let node_store = Arc::clone(&store);

    // Prepare SWAP settlement first: the provider must be embedded in the
    // accounting, and the swap event sink must be routed at node build time.
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

    // The peer manager behind the topology handle is the reporting authority:
    // accounting and the settlement services report violations through it so
    // misbehaving peers are scored down.
    let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

    let accounting_builder = AccountingBuilder::new(params.bandwidth.clone())
        .with_pricer_from_config(Arc::clone(params.spec))
        .with_reporter(Arc::clone(&reporter));
    #[cfg(feature = "swap")]
    let accounting = match swap_provider {
        Some(provider) => accounting_builder
            .with_settlement(provider)
            .build(params.identity),
        None => accounting_builder.build(params.identity),
    };
    #[cfg(not(feature = "swap"))]
    let accounting = accounting_builder.build(params.identity);
    // Share one accounting instance across the selector, the two-leg forwarder,
    // and the node task that keeps it alive.
    let accounting = Arc::new(accounting);

    // Enable multi-hop forwarding: a retrieval cache miss relays to a
    // strictly-closer peer and an inbound pushsync relays toward the chunk's
    // neighbourhood, accounting both legs over the same accounting instance the
    // origin path uses. Installed before the event loop accepts connections.
    node.enable_forwarding(
        Arc::new(topology.clone()),
        Arc::clone(&accounting),
        client_handle.clone(),
    );

    // Retrieval and pushsync candidate selection consults peer scores and
    // affordability on top of proximity order.
    let selector = Arc::new(PeerSelector::new(
        Arc::new(topology.clone()),
        accounting.bandwidth().clone(),
        Arc::new(accounting.pricing().clone()),
        Arc::new(AccountingSettlement::new(accounting.bandwidth().clone())),
    ));

    // Outbound self-throttle: pace our retrieval and pushsync requests under
    // each peer's pseudosettle allowance so a burst never crosses the remote's
    // settlement trigger. The allowance signal is the same `PeerAffordability`
    // the selector consults, built once in accounting. One bucket token is one
    // AU: the bucket refills at the pseudosettle per-second forgiveness rate
    // (`refresh_rate` AU/sec) and each request costs the exact per-chunk
    // proximity price the remote meters, taken from the same pricer the
    // accounting layer debits through, so a neighborhood chunk paces at the full
    // forgiveness rate while a distant one costs proportionally more. The bucket
    // is sized to a configurable percent of the headroom toward the payment
    // threshold, keeping a burst below the swap trigger with a margin to spare.
    let throttle = Arc::new(SelfThrottle::new(&accounting, params.bandwidth));
    let throttled_handle = client_handle.clone().with_throttle(Arc::clone(&throttle));

    let chunk_provider =
        NetworkChunkProvider::new(throttled_handle, topology.clone()).with_selector(selector);
    let chunks = VerifyingChunkProvider::new(chunk_provider, params.verify);

    // Spawn client service as independent task with graceful shutdown. The
    // client service reports retrieval and pushsync outcomes (success,
    // failure, and malformed-chunk invalid data) through the same peer manager
    // authority that accounting uses.
    // The service shares the same throttle instance the handle paces against, so
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

    // Spawn the SWAP settlement service over the shared accounting instance,
    // forwarding its cheque commands to the node and cashing received cheques
    // on chain when a provider is present.
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

    // Return node task - accounting is moved into the closure to keep it
    // alive. The chain provider is held alive the same way.
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
        let parts = build_client_backed_node(
            ctx,
            ClientNodeParams {
                node_type: SwarmNodeType::Client,
                spec: self.spec(),
                identity: self.identity(),
                network: self.network(),
                bandwidth: self.bandwidth(),
                verify: self.verify(),
                // The cache-only client builds its chunk cache over a
                // byte-bounded LRU; no reserve, signer, radius, or redb is
                // wired, so the opened database handle is ignored.
                make_store: Box::new(|_db| {
                    Ok(Arc::new(vertex_swarm_localstore::ChunkStore::with_budget(
                        vertex_swarm_localstore::DEFAULT_CACHE_BUDGET_BYTES as usize,
                        vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS,
                    )))
                }),
                #[cfg(feature = "chain")]
                chain: self.chain(),
                #[cfg(feature = "swap")]
                swap: self.swap(),
            },
        )
        .await?;

        let providers = construct::client(parts.topology, parts.chunks);
        Ok((parts.task, providers))
    }
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
        // The storer's local store is the persisting reserve. Its capacity is a
        // consensus quantity, not a function of local disk: the Swarm spec fixes
        // the reserve at a power-of-two number of chunks
        // (`DEFAULT_RESERVE_CAPACITY = 2^22`), and the redistribution game derives
        // the storage radius, and thence the committed depth it samples and
        // commits on chain, from occupancy relative to exactly that figure. Two
        // nodes covering the same neighbourhood must therefore agree on the
        // capacity regardless of how much disk each has provisioned, so the
        // capacity is read from the spec, never estimated from a byte budget (the
        // byte budget governs only the client cache and disk provisioning). Start
        // at radius 0 (responsible for everything until the radius manager raises
        // it); furthest-from-neighbourhood eviction sheds under pressure.
        // `redistribution_enabled` is read here so the storage config is no longer
        // discarded; the redistribution subsystem (which consumes it) is not part
        // of this train.
        let _redistribution_enabled = self.storage().redistribution_enabled();
        let capacity = self.spec().reserve_capacity;
        let identity = self.identity().clone();

        // The storer's local store is the persisting reserve, built over the
        // database the launch path opens. The launch path returns the same
        // erased handle the node holds (`Arc<dyn ReserveStore>` does not upcast
        // to `Arc<dyn SwarmLocalStore>`, so the concrete `Arc<DbReserve>` is
        // kept and the launch path erases the one `SwarmLocalStore` handle the
        // node and the components share). Serving-on-retrieval reads it because
        // the retrieval handler consults `SwarmLocalStore::get` before the
        // network.
        let parts = build_client_backed_node(
            ctx,
            ClientNodeParams {
                node_type: SwarmNodeType::Storer,
                spec: self.spec(),
                identity: self.identity(),
                network: self.network(),
                bandwidth: self.bandwidth(),
                verify: self.verify(),
                make_store: Box::new(move |db| build_storer_reserve(db, &identity, capacity)),
                #[cfg(feature = "chain")]
                chain: self.chain(),
                #[cfg(feature = "swap")]
                swap: self.swap(),
            },
        )
        .await?;

        let providers = construct::storer(parts.topology, parts.chunks, parts.store);
        Ok((parts.task, providers))
    }
}

/// Block confirmations a batch must accrue before the reserve will admit chunks
/// stamped under it.
///
/// Matches the intent's `blockThreshold` (`10`): a batch's creation must be
/// sufficiently confirmed on chain before it is usable, so a reorg cannot
/// retroactively invalidate admitted chunks. The chain-driven ingest (#391/#392)
/// will make this configurable once the postage indexer feeds a live
/// `PostageContext`; until then the reserve runs with no batches and admits
/// nothing, so the threshold is the admission policy that takes effect the moment
/// the batch store is populated.
const RESERVE_CONFIRMATION_THRESHOLD: u64 = 10;

/// Build the storer reserve over the shared database, erased to the local-store
/// trait.
///
/// Reuses the opened shared database when present so the reserve, its batch
/// store and the peer store share one handle; falls back to an in-memory redb
/// when the node runs without persistence, keeping the storer servable (the
/// reserve just does not survive a restart, matching the in-memory degradation
/// elsewhere). Both the in-memory open and the reserve's table creation are
/// surfaced as a build error rather than panicking.
///
/// The reserve now admits only stamped chunks, so it is built over two further
/// collaborators sharing the same database:
///
/// - a [`DbBatchStore`](vertex_swarm_postage::DbBatchStore), the authoritative
///   postage batch set and live [`PostageContext`] the admission policy reads;
/// - an [`AdmissionValidator`](vertex_swarm_postage::AdmissionValidator)
///   enforcing [`RESERVE_CONFIRMATION_THRESHOLD`] block confirmations plus the
///   structural and signature checks before a stamped chunk enters the reserve.
///
/// The batch store starts empty: until the postage indexer (#391/#392) feeds
/// `Created`/`TopUp`/`DepthIncrease`/`Expired` events the reserve admits nothing,
/// which is the correct conservative behaviour for a storer with no known
/// batches. The wiring is in place so populating the batch store is all that is
/// left to make the storer admit live traffic.
fn build_storer_reserve(
    db: Option<Arc<RedbDatabase>>,
    identity: &Arc<Identity>,
    capacity: u64,
) -> Result<Arc<dyn vertex_swarm_api::SwarmLocalStore>, SwarmNodeError> {
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
    // The batch store and the reserve share one database handle, so the postage
    // ingest seam and the reserve see a single consistent view.
    let batches =
        DbBatchStore::new(Arc::clone(&db)).map_err(|e| SwarmNodeError::Build(e.into()))?;
    let admission = AdmissionValidator::new(RESERVE_CONFIRMATION_THRESHOLD);
    let reserve = DbReserve::new(
        db,
        identity.as_ref(),
        batches,
        admission,
        capacity,
        EvictionStrategy::EvictFurthest,
        StorageRadius::ZERO,
    )
    .map_err(|e| SwarmNodeError::Build(e.into()))?;
    Ok(Arc::new(reserve))
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

    /// The launch path hands the peer manager to the accounting builder as the
    /// peer reporter. Verify that composition end to end: an accounting
    /// violation must reach the peer manager and lower the peer's score.
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

    /// The storer store factory builds the persisting reserve (not the
    /// cache-only client store) and falls back to an in-memory database when
    /// persistence is not configured.
    ///
    /// The reworked reserve admits only stamped chunks whose batch is known and
    /// whose stamp passes the admission validator the factory wires. This test
    /// owns the *wiring* contract: that the factory yields a working
    /// `SwarmLocalStore` handle, that it is the admission-gated reserve (a put
    /// for an unknown batch is rejected, proving admission is wired and not
    /// bypassed), and that the handle serves back what the reserve holds. The
    /// full admissible-put path (batch registration, signature recovery, owner
    /// match) is exercised exhaustively by the reserve crate's own tests, which
    /// can reach the batch store the factory deliberately erases; reproducing it
    /// here would only re-test the reserve through a narrower seam.
    #[test]
    fn storer_reserve_factory_builds_admission_gated_store() {
        use alloy_primitives::{B256, Signature};
        use nectar_postage::Stamp;
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let identity = test_identity_arc();
        // The reserve capacity is a consensus power-of-two chunk count (the spec
        // fixes the default at 2^22); a small power of two keeps the test cheap
        // while matching that shape.
        let capacity: u64 = 1 << 12;

        // db = None exercises the in-memory fallback.
        let store = build_storer_reserve(None, &identity, capacity).expect("reserve builds");

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

        // A stamped put whose batch is unknown to the (empty) batch store is
        // rejected by the wired admission validator. This is the load-bearing
        // assertion: it proves the factory built the admission-gated reserve and
        // did not fall back to a store that accepts arbitrary chunks.
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
}
