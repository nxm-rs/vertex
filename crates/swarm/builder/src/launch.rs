//! SwarmLaunchConfig implementations for config types.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use vertex_net_peer_store::PeerSnapshotStore;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_api::{
    PeerReporter, SwarmClientAccounting, SwarmLaunchConfig, SwarmNodeType, SwarmSpec,
};
use vertex_swarm_bandwidth::{
    Accounting, AccountingBuilder, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_node::{AccountingSettlement, BootNode, ClientNode, PeerSelector};
use vertex_swarm_peer_manager::{
    DEFAULT_TICK_INTERVAL, DbPeerSnapshotStore, PeerSnapshot, spawn_peer_manager_task,
};
use vertex_swarm_spec::{Loggable, Spec};
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::{GracefulShutdown, NodeTaskFn};

use crate::config::{BootnodeConfig, ClientConfig, StorerConfig};
use crate::error::SwarmNodeError;
use crate::providers::NetworkChunkProvider;
use crate::rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};
use crate::verify::{ChunkVerifyConfig, VerifyingChunkProvider};

/// Network chunk provider wrapped with config-gated download verification.
type VerifiedChunkProvider = VerifyingChunkProvider<NetworkChunkProvider<Arc<Identity>>>;

#[cfg(feature = "chain")]
use vertex_swarm_api::SwarmAccountingConfig;
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

/// Borrowed inputs for [`build_client_backed_node`], gathered from a validated
/// client or storer config.
struct ClientNodeParams<'a> {
    node_type: SwarmNodeType,
    spec: &'a Arc<Spec>,
    identity: &'a Arc<Identity>,
    network: &'a NetworkConfig<KademliaConfig>,
    bandwidth: &'a DefaultBandwidthConfig,
    verify: ChunkVerifyConfig,
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

    // The cache-only client builds its chunk cache over a byte-bounded LRU and
    // injects it; the same cache serves inbound retrievals and holds the
    // client's own deliveries. No reserve, signer, radius, or redb is wired.
    let store: Arc<dyn vertex_swarm_api::SwarmLocalStore> =
        Arc::new(vertex_swarm_localstore::ChunkStore::with_budget(
            vertex_swarm_localstore::DEFAULT_CACHE_BUDGET_BYTES as usize,
            vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS,
        ));
    let node_builder = ClientNode::builder(params.identity.clone()).with_store(store);
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
        SwarmSpec::network_id(params.spec.as_ref()),
        Arc::clone(&reporter),
    );

    // Retrieval and pushsync candidate selection consults peer scores and
    // affordability on top of proximity order.
    let selector = Arc::new(PeerSelector::new(
        Arc::new(topology.clone()),
        accounting.bandwidth().clone(),
        Arc::new(accounting.pricing().clone()),
        Arc::new(AccountingSettlement::new(accounting.bandwidth().clone())),
    ));

    let chunk_provider =
        NetworkChunkProvider::new(client_handle.clone(), topology.clone()).with_selector(selector);
    let chunks = VerifyingChunkProvider::new(chunk_provider, params.verify);

    // Spawn client service as independent task with graceful shutdown. The
    // client service reports retrieval and pushsync outcomes (success,
    // failure, and malformed-chunk invalid data) through the same peer manager
    // authority that accounting uses.
    let client_service = client_service.with_reporter(Arc::clone(&reporter));
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
    })
}

impl SwarmLaunchConfig for BootnodeConfig {
    type Types = BootnodeLaunchTypes;
    type Providers = BootnodeRpcProviders<Arc<Identity>>;
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
        let providers = BootnodeRpcProviders::new(topology);

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
    type Providers = ClientRpcProviders<Arc<Identity>, VerifiedChunkProvider>;
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
                #[cfg(feature = "chain")]
                chain: self.chain(),
                #[cfg(feature = "swap")]
                swap: self.swap(),
            },
        )
        .await?;

        let providers = ClientRpcProviders::new(parts.topology, parts.chunks);
        Ok((parts.task, providers))
    }
}

impl SwarmLaunchConfig for StorerConfig {
    type Types = StorerLaunchTypes;
    type Providers = StorerRpcProviders<Arc<Identity>, VerifiedChunkProvider>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        // TODO: build storer-specific components
        let _ = self.local_store();
        let _ = self.storage();

        // Built over the client launch path for now (storer components not yet
        // implemented).
        let parts = build_client_backed_node(
            ctx,
            ClientNodeParams {
                node_type: SwarmNodeType::Storer,
                spec: self.spec(),
                identity: self.identity(),
                network: self.network(),
                bandwidth: self.bandwidth(),
                verify: self.verify(),
                #[cfg(feature = "chain")]
                chain: self.chain(),
                #[cfg(feature = "swap")]
                swap: self.swap(),
            },
        )
        .await?;

        let providers = StorerRpcProviders::new(parts.topology, parts.chunks);
        Ok((parts.task, providers))
    }
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
}
