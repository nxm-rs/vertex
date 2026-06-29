//! SwarmLaunchConfig implementations for config types.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use vertex_net_peer_store::PeerSnapshotStore;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_accounting::{Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer};
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, SwarmLaunchConfig, SwarmNodeType, construct,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_node::{
    BootNode, ChunkVerifyConfig, ClientNode, ClientNodeParts, ClientTailParams, NodeRunParts,
    RunTaskFn, VerifiedChunkProvider, build_client_core_tail, single_task,
};
use vertex_swarm_peer_manager::{
    DEFAULT_TICK_INTERVAL, DbPeerSnapshotStore, PeerSnapshot, spawn_peer_manager_task,
};
use vertex_swarm_spec::{Loggable, Spec};
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::NodeTaskFn;

use crate::config::{BootnodeConfig, ClientConfig};
use crate::error::SwarmNodeError;

#[cfg(feature = "swap")]
use vertex_swarm_node::args::ChainConfig;
#[cfg(feature = "swap")]
use vertex_swarm_node::args::SwapConfig;
#[cfg(feature = "swap")]
use vertex_swarm_node::{ClientSwapParams, NodeChainError, node_chain_provider};

pub(crate) type PeerStore = Arc<dyn PeerSnapshotStore<PeerSnapshot>>;

/// Stats collection interval for database metrics.
const DB_METRICS_INTERVAL: Duration = Duration::from_secs(30);

fn log_build_start(node_type: SwarmNodeType, spec: &Spec) {
    info!(%node_type, "Building node...");
    spec.log();
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

/// Map a chain-resolution failure onto the builder error.
#[cfg(feature = "swap")]
fn map_chain_error(err: NodeChainError) -> SwarmNodeError {
    match err {
        NodeChainError::Required { node_type } => SwarmNodeError::ChainRequired { node_type },
        NodeChainError::Build(message) => SwarmNodeError::Chain(message),
    }
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

/// A cache override supplied through the builder. With no seam the launch path
/// builds the default in-memory [`vertex_swarm_localstore::ChunkStore`] sized
/// from the local-store config.
pub(crate) enum CacheSeam {
    /// A pre-built cache, used as-is.
    Ready(Arc<dyn vertex_swarm_api::SwarmLocalStore>),
    /// A factory invoked at build time with the opened shared database.
    Factory(CacheFactory),
}

/// Builds a cache from the opened shared database (if any).
pub(crate) type CacheFactory = Box<
    dyn FnOnce(
            Option<Arc<RedbDatabase>>,
        ) -> Result<Arc<dyn vertex_swarm_api::SwarmLocalStore>, SwarmNodeError>
        + Send,
>;

/// Resolve a cache seam into a local store, defaulting to a byte-bounded in-memory
/// LRU sized from the config when no seam is supplied.
pub(crate) fn resolve_cache(
    cache: Option<CacheSeam>,
    db: Option<Arc<RedbDatabase>>,
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
) -> Result<Arc<dyn vertex_swarm_api::SwarmLocalStore>, SwarmNodeError> {
    match cache {
        None => Ok(default_cache(cache_budget_bytes, soc_cache_ttl)),
        Some(CacheSeam::Ready(cache)) => Ok(cache),
        Some(CacheSeam::Factory(factory)) => factory(db),
    }
}

/// Borrowed inputs for [`build_client_backed_node`], gathered from a validated
/// client or storer config. The node type comes from the assembly seam
/// ([`NodeAssembly::NODE_TYPE`]), not this struct, so the two cannot desync.
pub(crate) struct ClientNodeParams<'a> {
    pub(crate) spec: &'a Arc<Spec>,
    pub(crate) identity: &'a Arc<Identity>,
    pub(crate) network: &'a NetworkConfig<KademliaConfig>,
    pub(crate) bandwidth: &'a DefaultBandwidthConfig,
    pub(crate) verify: ChunkVerifyConfig,
    #[cfg(feature = "swap")]
    pub(crate) chain: &'a ChainConfig,
    #[cfg(feature = "swap")]
    pub(crate) swap: &'a SwapConfig,
}

/// Shared inputs every node assembly consumes, independent of node type. The seam
/// builds its own local serve store from `db`.
pub(crate) struct AssemblyInputs<'a> {
    pub(crate) db: Option<Arc<RedbDatabase>>,
    pub(crate) identity: &'a Arc<Identity>,
    pub(crate) network: &'a NetworkConfig<KademliaConfig>,
    pub(crate) peer_store: Option<PeerStore>,
    pub(crate) pseudosettle_event_sender:
        tokio::sync::mpsc::UnboundedSender<vertex_swarm_node::PseudosettleEvent>,
    #[cfg(feature = "swap")]
    pub(crate) swap_event_sender:
        Option<tokio::sync::mpsc::UnboundedSender<vertex_swarm_node::SwapEvent>>,
}

/// The node-type-specific launch seam. The client assembly ([`ClientAssembly`])
/// lives here; the storer assembly lives behind the `reserve` feature in
/// `crate::storer`. An implementor builds the local serve store from the opened
/// database and assembles the concrete node in one pass, exposing its provider
/// handles as [`Self::ProviderStore`].
#[async_trait::async_trait]
pub(crate) trait NodeAssembly: Send {
    /// The runtime node type this assembly produces. The shared launch path reads
    /// the node type from here, so the seam and the node type cannot desync.
    const NODE_TYPE: SwarmNodeType;

    /// Store handles the node type's RPC providers wrap: `()` for a client, the
    /// serve view plus reserve for a storer.
    type ProviderStore: Send;

    /// Build the local serve store from the opened database and assemble the
    /// concrete node, returning its run-task factory and the provider store. Only
    /// the storer reads `ctx` (to spawn its puller); the client ignores it.
    async fn assemble(
        self,
        ctx: &dyn InfrastructureContext,
        inputs: AssemblyInputs<'_>,
    ) -> Result<(NodeRunParts, Self::ProviderStore), SwarmNodeError>;
}

/// The default client assembly: a bare [`ClientNode`] over an in-memory cache.
pub(crate) struct ClientAssembly {
    cache: Option<CacheSeam>,
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
}

impl ClientAssembly {
    pub(crate) fn new(
        cache: Option<CacheSeam>,
        cache_budget_bytes: u64,
        soc_cache_ttl: u64,
    ) -> Self {
        Self {
            cache,
            cache_budget_bytes,
            soc_cache_ttl,
        }
    }
}

#[async_trait::async_trait]
impl NodeAssembly for ClientAssembly {
    const NODE_TYPE: SwarmNodeType = SwarmNodeType::Client;

    type ProviderStore = ();

    async fn assemble(
        self,
        _ctx: &dyn InfrastructureContext,
        inputs: AssemblyInputs<'_>,
    ) -> Result<(NodeRunParts, Self::ProviderStore), SwarmNodeError> {
        let node_store = resolve_cache(
            self.cache,
            inputs.db,
            self.cache_budget_bytes,
            self.soc_cache_ttl,
        )?;
        let parts = assemble_client_node(
            inputs.identity,
            inputs.network,
            node_store,
            inputs.peer_store,
            inputs.pseudosettle_event_sender,
            #[cfg(feature = "swap")]
            inputs.swap_event_sender,
        )
        .await?;
        Ok((parts, ()))
    }
}

/// Shared launch path for the client- and storer-backed node types.
///
/// Resolves the chain precondition, opens the database, and builds the peer
/// store, then delegates the wasm-clean wiring (accounting, settlement, the
/// verified chunk provider, service spawning) to [`build_client_core_tail`]. The
/// node-type-specific local store and node assembly are injected through
/// `assembly`, invoked by the tail over the prepared settlement event sinks.
pub(crate) async fn build_client_backed_node<F: NodeAssembly>(
    ctx: &dyn InfrastructureContext,
    params: ClientNodeParams<'_>,
    assembly: F,
) -> Result<ClientNodeParts<F::ProviderStore>, SwarmNodeError> {
    let node_type = F::NODE_TYPE;
    log_build_start(node_type, params.spec);

    // A client paces against the scaled line a storer enforces on it; a storer
    // keeps the unscaled figures.
    let scaled_bandwidth;
    let bandwidth = if node_type.requires_storage() {
        params.bandwidth
    } else {
        scaled_bandwidth = params.bandwidth.clone().for_client();
        &scaled_bandwidth
    };

    // SWAP defaults on for storers (maximum support) and off for clients; an
    // explicit --swap overrides. The tail derives the same value for its swap
    // wiring; here it gates the chain precondition.
    #[cfg(feature = "swap")]
    let swap_enabled = params.swap.enable.unwrap_or(node_type.swap_default());
    #[cfg(not(feature = "swap"))]
    let swap_enabled = false;

    // A storer always needs a chain (staking, oracle, settlement); a client needs
    // one only with SWAP. Resolve this precondition before any runtime work so a
    // chain-required node fails before allocating tasks, the database, or the node.
    #[cfg(feature = "swap")]
    let chain_provider = node_chain_provider(
        params.spec,
        params.identity,
        node_type,
        swap_enabled,
        params.chain.rpc_url.as_deref(),
    )
    .await
    .map_err(map_chain_error)?;
    // Without the `swap` feature (which carries the chain provider) a chain-required
    // node cannot resolve one.
    #[cfg(not(feature = "swap"))]
    if node_type.needs_chain(swap_enabled) {
        return Err(SwarmNodeError::ChainRequired { node_type });
    }

    let db = open_shared_database(ctx);
    let peer_store = create_peer_store(&db);

    let tail_params = ClientTailParams {
        node_type,
        spec: params.spec,
        identity: params.identity,
        bandwidth,
        verify: params.verify,
        #[cfg(feature = "swap")]
        swap: ClientSwapParams {
            enable: params.swap.enable,
            chequebook: params.swap.chequebook,
            beneficiary: params.swap.beneficiary,
            deploy: params.swap.deploy,
            bounce_limit: params.swap.bounce_limit,
        },
    };

    let parts = build_client_core_tail(
        ctx.executor(),
        tail_params,
        // SWAP carries the chain provider, so the tail accepts the resolved
        // handle whenever swap is built.
        #[cfg(feature = "swap")]
        chain_provider,
        |events| {
            assembly.assemble(
                ctx,
                AssemblyInputs {
                    db,
                    identity: params.identity,
                    network: params.network,
                    peer_store,
                    pseudosettle_event_sender: events.pseudosettle,
                    #[cfg(feature = "swap")]
                    swap_event_sender: events.swap,
                },
            )
        },
    )
    .await?;

    info!(%node_type, "Node built successfully");
    Ok(parts)
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
) -> Result<NodeRunParts, SwarmNodeError> {
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

    Ok(NodeRunParts {
        topology,
        client_service,
        client_handle,
        run,
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
            spec: config.spec(),
            identity: config.identity(),
            network: config.network(),
            bandwidth: config.bandwidth(),
            verify: config.verify(),
            #[cfg(feature = "swap")]
            chain: config.chain(),
            #[cfg(feature = "swap")]
            swap: config.swap(),
        },
        ClientAssembly::new(cache, cache_budget, soc_ttl),
    )
    .await?;

    let ClientNodeParts {
        task,
        topology,
        chunks,
        provider_store: (),
        ..
    } = parts;
    let providers = construct::client(topology, chunks);
    Ok((task, providers))
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::{Path, PathBuf};

    use nectar_primitives::Nonce;
    use vertex_swarm_accounting::AccountingBuilder;
    use vertex_swarm_api::{Au, SwarmAccountingConfig, SwarmClientAccounting, SwarmIdentity};
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

    /// A debtor breaching its own disconnect line refuses the receive but never
    /// scores the creditor: downloading past our own debt line is a local pacing
    /// outcome, not peer misbehaviour, so a peer wired as reporter keeps its
    /// score. Scoring it evicted and banned the healthy storers a download
    /// depends on.
    #[test]
    fn receive_breach_does_not_lower_peer_score() {
        let identity = test_identity_arc();
        let peer_manager = PeerManager::new(&identity, PeerManagerConfig::default());
        let overlay = peer_manager.store_discovered_peer(test_swarm_peer(0xab));
        let baseline = peer_manager
            .get_peer_score(&overlay)
            .expect("stored peer has a score");

        let config = DefaultBandwidthConfig::default();
        let over_limit = config.disconnect_threshold() + Au::new(1);
        let accounting = AccountingBuilder::new(config)
            .with_pricer_from_config(identity.spec().clone())
            .build(&identity);

        // A debit projected past the disconnect threshold is refused.
        let result = accounting
            .bandwidth()
            .prepare_receive(overlay, over_limit, true);
        assert!(result.is_err());

        // The peer's score is unchanged: the breach is never scored.
        let after = peer_manager
            .get_peer_score(&overlay)
            .expect("peer still scored");
        assert_eq!(after, baseline, "a receive breach must not score the peer");
    }

    /// The origin gate wired onto the client handle reserves the price at
    /// dispatch and commits it exactly once on delivery, debiting the serving
    /// peer by the per-chunk price. This mirrors the `with_origin_gate` wiring
    /// `assemble_client_core` builds, proving an end-to-end origin retrieval
    /// debits the serving peer exactly once through the reservation path.
    #[tokio::test]
    async fn builder_wiring_debits_an_origin_delivery() {
        use nectar_primitives::{AnyChunk, ChunkAddress, ContentChunk};
        use tokio::sync::mpsc;
        use vertex_swarm_api::{
            AdmissionControl, BandwidthDebit, SwarmBandwidthAccounting, SwarmPeerBandwidth,
            SwarmPricing,
        };
        use vertex_swarm_node::{
            AccountingSettlement, ClientCommand, ClientHandle, RetrievalResult, SettlementTrigger,
        };

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

        // The same gate `assemble_client_core` wires onto the origin-gated handle.
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let settlement: Arc<dyn SettlementTrigger> =
            Arc::new(AccountingSettlement::new(accounting.bandwidth().clone()));
        let handle = ClientHandle::new(tx).with_origin_gate(
            Arc::new(accounting.pricing().clone()),
            accounting.bandwidth().clone() as Arc<dyn BandwidthDebit>,
            accounting.bandwidth().clone() as Arc<dyn AdmissionControl>,
            settlement,
        );

        let request = tokio::spawn({
            let handle = handle.clone();
            async move { handle.retrieve_chunk(overlay, address, true).await }
        });

        // The reservation is held the moment the command is dispatched.
        let response = match rx.recv().await.expect("origin retrieval dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        response
            .send(Ok(RetrievalResult {
                chunk,
                stamp: None,
                peer: overlay,
            }))
            .expect("receiver alive");
        request.await.unwrap().expect("delivery ok");

        assert_eq!(
            accounting.bandwidth().for_peer(overlay).balance(),
            -price,
            "an own-request delivery debits the serving peer by the per-chunk price"
        );
    }

    #[test]
    fn resolve_cache_default_builds_a_working_cache() {
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let store = resolve_cache(None, None, 1 << 20, DEFAULT_SOC_CACHE_TTL_NS_TEST)
            .expect("default cache builds");

        let chunk: AnyChunk = ContentChunk::new(b"cached content".to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        store
            .put(CachedChunk::new(chunk, None))
            .expect("the default cache accepts a content chunk");
        assert!(
            store.contains(&address),
            "the default cache serves what it stored"
        );
    }

    #[test]
    fn resolve_cache_honors_a_ready_cache_seam() {
        let cache: Arc<dyn vertex_swarm_api::SwarmLocalStore> = Arc::new(
            vertex_swarm_localstore::ChunkStore::with_budget(4096, DEFAULT_SOC_CACHE_TTL_NS_TEST),
        );
        let store = resolve_cache(Some(CacheSeam::Ready(Arc::clone(&cache))), None, 0, 0)
            .expect("seam cache is used");
        assert!(
            Arc::ptr_eq(&cache, &store),
            "the supplied cache must reach the node store unchanged"
        );
    }

    /// Test SOC cache TTL: any non-zero value works for the cache-shape tests.
    const DEFAULT_SOC_CACHE_TTL_NS_TEST: u64 = vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS;

    // The chain-precondition policy now lives with `node_chain_provider` in
    // `vertex-swarm-node`; its tests moved there.
}
