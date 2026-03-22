//! SwarmLaunchConfig implementation for progressive building.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{info, warn};

use vertex_net_peer_store::NetPeerStore;
use vertex_net_peer_store::error::StoreError;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_api::{SwarmLaunchConfig, SwarmNodeType, SwarmPeerConfig, SwarmScoreStore};
use vertex_swarm_bandwidth::{
    Accounting, AccountingBuilder, BandwidthConfig, ClientAccounting, DbAccountingStore,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::ClientNode;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_peer_manager::{DbPeerStore, StoredPeer};
use vertex_swarm_peer_score::PeerScore;
use vertex_swarm_spec::{Loggable, Spec};
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::{GracefulShutdown, NodeTaskFn};

use crate::config::{SwarmBuildConfig, SwarmConfigError};
use crate::error::SwarmNodeError;
use crate::node::SwarmProtocolBuilder;
use crate::providers::NetworkChunkProvider;
use crate::rpc::{FullRpcProviders, SwarmNodeProviders};

type PeerStore = Arc<dyn NetPeerStore<StoredPeer>>;
type PeerScoreStore = Arc<dyn SwarmScoreStore<Value = PeerScore, Error = StoreError>>;

/// Stats collection interval for database metrics.
const DB_METRICS_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) fn log_build_start<N: SwarmPeerConfig>(node_type: &str, spec: &Spec, network: &N) {
    info!("Building {} node...", node_type);
    spec.log();

    match network.store_path() {
        Some(path) => info!(path = %path.display(), "Peers database"),
        None => info!("Peers database: ephemeral"),
    }
}

pub(crate) fn build_accounting(
    spec: Arc<Spec>,
    identity: &Arc<Identity>,
    config: BandwidthConfig,
    store: Option<Arc<DbAccountingStore<RedbDatabase>>>,
) -> ClientAccounting<Arc<Accounting<BandwidthConfig, Arc<Identity>>>, Spec> {
    let mut builder = AccountingBuilder::new(config);

    if let Some(store) = store {
        builder = builder.with_store(store);
    }

    builder.build(spec, identity)
}

/// Wrap a future factory as a NodeTaskFn with graceful shutdown support.
pub(crate) fn single_task<F, Fut>(f: F) -> NodeTaskFn
where
    F: FnOnce(GracefulShutdown) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Box::new(move |shutdown| Box::pin(f(shutdown)))
}

pub(crate) fn open_shared_database(ctx: &dyn InfrastructureContext) -> Option<Arc<RedbDatabase>> {
    let path = ctx.data_dir().join("db").join("vertex.redb");
    match vertex_storage_redb::open_database(Some(&path), false) {
        Ok(db) => {
            info!(path = %path.display(), "Shared database opened");
            spawn_db_metrics_task(ctx, db.clone());
            Some(db)
        }
        Err(e) => {
            warn!(error = %e, "Failed to open shared database");
            None
        }
    }
}

fn spawn_db_metrics_task(ctx: &dyn InfrastructureContext, db: Arc<RedbDatabase>) {
    ctx.executor()
        .spawn_with_graceful_shutdown_signal("db.metrics", move |shutdown| async move {
            let mut shutdown = std::pin::pin!(shutdown);
            let mut interval = tokio::time::interval(DB_METRICS_INTERVAL);

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

pub(crate) fn create_accounting_store(
    db: &Option<Arc<RedbDatabase>>,
) -> Option<Arc<DbAccountingStore<RedbDatabase>>> {
    let db = db.as_ref()?;
    let store = Arc::new(DbAccountingStore::new(Arc::clone(db)));
    match store.init() {
        Ok(()) => {
            info!("Accounting store: shared database");
            Some(store)
        }
        Err(e) => {
            warn!(error = %e, "Failed to init accounting table");
            None
        }
    }
}

pub(crate) fn create_peer_store(
    db: &Option<Arc<RedbDatabase>>,
) -> (Option<PeerStore>, Option<PeerScoreStore>) {
    let Some(db) = db.as_ref() else {
        return (None, None);
    };
    let store = Arc::new(DbPeerStore::new(db.clone()));
    match store.init() {
        Ok(()) => {
            info!("Peer store: shared database");
            let peer_store: PeerStore = Arc::clone(&store) as _;
            let score_store: PeerScoreStore = store as _;
            (Some(peer_store), Some(score_store))
        }
        Err(e) => {
            warn!(error = %e, "Failed to init peer table");
            (None, None)
        }
    }
}

/// Build a client-like node (client or storer) with shared infrastructure.
///
/// Both client and storer nodes share identical build logic: open DB, create
/// stores, build accounting, construct a `ClientNode`, extract topology,
/// create chunk provider, spawn client service, and set up shutdown with
/// accounting flush.
pub(crate) async fn build_client_like_node(
    node_type: &'static str,
    spec: &Arc<Spec>,
    identity: &Arc<Identity>,
    network: &NetworkConfig<KademliaConfig>,
    bandwidth: BandwidthConfig,
    ctx: &dyn InfrastructureContext,
) -> Result<(NodeTaskFn, FullRpcProviders<Arc<Identity>, NetworkChunkProvider<Arc<Identity>>>), SwarmNodeError>
{
    let db = open_shared_database(ctx);
    let (peer_store, score_store) = create_peer_store(&db);
    let accounting_store = create_accounting_store(&db);

    let accounting = build_accounting(
        spec.clone(),
        identity,
        bandwidth,
        accounting_store,
    );

    let (node, client_service, client_handle) = ClientNode::builder(identity.clone())
        .build(network, peer_store, score_store)
        .await
        .map_err(|e| SwarmNodeError::Build(e.into()))?;

    let topology = node.topology_handle().clone();
    let chunk_provider = NetworkChunkProvider::new(client_handle, topology.clone());
    let providers = FullRpcProviders::new(topology, chunk_provider);

    // Spawn client service as independent task with graceful shutdown
    ctx.executor()
        .spawn_service("swarm.client_service", client_service);

    // Return node task - accounting is moved into the closure to keep it alive.
    // On shutdown: flush peer state to the store.
    // TODO: Settle all outstanding balances before flushing. This requires
    // researching how Bee handles graceful settlement on shutdown (iterate
    // connected peers, call settle() for each with outstanding debt).
    let task = single_task(move |shutdown| async move {
        if let Err(e) = node.start_and_run(shutdown).await {
            tracing::error!(error = %e, "{} error", node_type);
        }
        if let Err(e) = accounting.bandwidth().flush_to_store() {
            tracing::warn!(error = %e, "Failed to flush accounting state on shutdown");
        }
    });

    info!("{} node built successfully", node_type);
    Ok((task, providers))
}

macro_rules! define_launch_types {
    ($name:ident) => {
        pub struct $name;

        impl vertex_swarm_api::SwarmPrimitives for $name {
            type Spec = Arc<Spec>;
            type Identity = Arc<Identity>;
        }

        impl vertex_swarm_api::SwarmNetworkTypes for $name {
            type Topology = TopologyHandle<Arc<Identity>>;
        }
    };
    ($name:ident, with_client) => {
        define_launch_types!($name);

        impl vertex_swarm_api::SwarmClientTypes for $name {
            type Accounting =
                ClientAccounting<Arc<Accounting<BandwidthConfig, Arc<Identity>>>, Spec>;
        }
    };
}

define_launch_types!(BootnodeLaunchTypes);
define_launch_types!(ClientLaunchTypes, with_client);

#[async_trait]
impl SwarmLaunchConfig for SwarmBuildConfig {
    type Types = ClientLaunchTypes;
    type Providers = SwarmNodeProviders;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        // Progressive validation: each step validates its input before
        // feeding it to the next builder stage.
        let network = self
            .protocol
            .network_config()
            .map_err(SwarmConfigError::from)?;

        let identity = self
            .protocol
            .identity(self.spec.clone(), &self.network_dir)
            .map_err(SwarmConfigError::Identity)?;

        let base = SwarmProtocolBuilder::with_context(ctx)
            .with_spec(self.spec)
            .with_identity(identity)
            .with_network(network);

        match self.protocol.node_type {
            SwarmNodeType::Bootnode => {
                let (task, providers) = base.build().await?.into_parts();
                Ok((task, SwarmNodeProviders::Bootnode(providers)))
            }
            SwarmNodeType::Client | SwarmNodeType::Storer => {
                let bandwidth = self
                    .protocol
                    .bandwidth_config()
                    .map_err(SwarmConfigError::from)?;

                let client = base.with_accounting(bandwidth);

                let (task, providers) = if self.protocol.node_type == SwarmNodeType::Storer {
                    let local_store = self.protocol.local_store_config();
                    let storage = self.protocol.storage_config();
                    client
                        .with_storage(local_store, storage)
                        .build()
                        .await?
                        .into_parts()
                } else {
                    client.build().await?.into_parts()
                };

                Ok((task, SwarmNodeProviders::Full(providers)))
            }
        }
    }
}
