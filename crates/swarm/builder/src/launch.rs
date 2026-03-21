//! SwarmLaunchConfig implementations for config types.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{info, warn};

use vertex_net_peer_store::NetPeerStore;
use vertex_net_peer_store::error::StoreError;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_api::{SwarmLaunchConfig, SwarmPeerConfig, SwarmScoreStore};
use vertex_swarm_bandwidth::{
    Accounting, AccountingBuilder, BandwidthConfig, ClientAccounting, DbAccountingStore,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{BootNode, ClientNode};
use vertex_swarm_peer_manager::{DbPeerStore, StoredPeer};
use vertex_swarm_peer_score::PeerScore;
use vertex_swarm_spec::{Loggable, Spec};
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::{GracefulShutdown, NodeTaskFn};

use crate::config::{BootnodeConfig, ClientConfig, StorerConfig};
use crate::error::SwarmNodeError;
use crate::providers::NetworkChunkProvider;
use crate::rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

type PeerStore = Arc<dyn NetPeerStore<StoredPeer>>;
type PeerScoreStore = Arc<dyn SwarmScoreStore<Value = PeerScore, Error = StoreError>>;

/// Stats collection interval for database metrics.
const DB_METRICS_INTERVAL: Duration = Duration::from_secs(30);

fn log_build_start<N: SwarmPeerConfig>(node_type: &str, spec: &Spec, network: &N) {
    info!("Building {} node...", node_type);
    spec.log();

    match network.store_path() {
        Some(path) => info!(path = %path.display(), "Peers database"),
        None => info!("Peers database: ephemeral"),
    }
}

fn build_accounting(
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
fn single_task<F, Fut>(f: F) -> NodeTaskFn
where
    F: FnOnce(GracefulShutdown) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Box::new(move |shutdown| Box::pin(f(shutdown)))
}

fn open_shared_database(ctx: &dyn InfrastructureContext) -> Option<Arc<RedbDatabase>> {
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

fn create_accounting_store(
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

fn create_peer_store(
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
define_launch_types!(StorerLaunchTypes, with_client);

#[async_trait]
impl SwarmLaunchConfig for BootnodeConfig {
    type Types = BootnodeLaunchTypes;
    type Providers = BootnodeRpcProviders<Arc<Identity>>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start("Bootnode", self.spec(), self.network());

        let db = open_shared_database(ctx);
        let (peer_store, score_store) = create_peer_store(&db);

        let node = BootNode::builder(self.identity().clone())
            .build(self.network(), peer_store, score_store)
            .await
            .map_err(|e| SwarmNodeError::Build(e.into()))?;

        let topology = node.topology_handle().clone();
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

#[async_trait]
impl SwarmLaunchConfig for ClientConfig {
    type Types = ClientLaunchTypes;
    type Providers = ClientRpcProviders<Arc<Identity>, NetworkChunkProvider<Arc<Identity>>>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start("Client", self.spec(), self.network());

        let db = open_shared_database(ctx);
        let (peer_store, score_store) = create_peer_store(&db);
        let accounting_store = create_accounting_store(&db);

        let accounting = build_accounting(
            self.spec().clone(),
            self.identity(),
            self.bandwidth().clone(),
            accounting_store,
        );

        let (node, client_service, client_handle) = ClientNode::builder(self.identity().clone())
            .build(self.network(), peer_store, score_store)
            .await
            .map_err(|e| SwarmNodeError::Build(e.into()))?;

        let topology = node.topology_handle().clone();
        let chunk_provider = NetworkChunkProvider::new(client_handle, topology.clone());
        let providers = ClientRpcProviders::new(topology, chunk_provider);

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
                tracing::error!(error = %e, "ClientNode error");
            }
            if let Err(e) = accounting.bandwidth().flush_to_store() {
                tracing::warn!(error = %e, "Failed to flush accounting state on shutdown");
            }
        });

        info!("Client node built successfully");
        Ok((task, providers))
    }
}

#[async_trait]
impl SwarmLaunchConfig for StorerConfig {
    type Types = StorerLaunchTypes;
    type Providers = StorerRpcProviders<Arc<Identity>, NetworkChunkProvider<Arc<Identity>>>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start("Storer", self.spec(), self.network());

        let db = open_shared_database(ctx);
        let (peer_store, score_store) = create_peer_store(&db);
        let accounting_store = create_accounting_store(&db);

        let accounting = build_accounting(
            self.spec().clone(),
            self.identity(),
            self.bandwidth().clone(),
            accounting_store,
        );

        // TODO: build storer-specific components
        let _ = self.local_store();
        let _ = self.storage();

        // Build as ClientNode for now (storer components not yet implemented)
        let (node, client_service, client_handle) = ClientNode::builder(self.identity().clone())
            .build(self.network(), peer_store, score_store)
            .await
            .map_err(|e| SwarmNodeError::Build(e.into()))?;

        let topology = node.topology_handle().clone();
        let chunk_provider = NetworkChunkProvider::new(client_handle, topology.clone());
        let providers = StorerRpcProviders::new(topology, chunk_provider);

        // Spawn client service as independent task with graceful shutdown
        ctx.executor()
            .spawn_service("swarm.client_service", client_service);

        // Return node task - accounting is moved into the closure to keep it alive.
        // On shutdown: flush peer state to the store.
        // TODO: Settle all outstanding balances before flushing (see ClientConfig).
        let task = single_task(move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "StorerNode error");
            }
            if let Err(e) = accounting.bandwidth().flush_to_store() {
                tracing::warn!(error = %e, "Failed to flush accounting state on shutdown");
            }
        });

        info!("Storer node built successfully");
        Ok((task, providers))
    }
}
