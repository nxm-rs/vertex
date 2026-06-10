//! SwarmLaunchConfig implementations for config types.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use vertex_net_peer_store::PeerSnapshotStore;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
#[cfg(feature = "swap")]
use vertex_swarm_api::SwarmClientAccounting;
use vertex_swarm_api::{PeerConfigValues, SwarmLaunchConfig, SwarmNodeType, SwarmPeerConfig};
use vertex_swarm_bandwidth::{
    Accounting, AccountingBuilder, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_node::{BootNode, ClientNode};
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
use vertex_swarm_api::{SwarmAccountingConfig, SwarmSpec};
#[cfg(feature = "chain")]
use vertex_swarm_node::args::ChainConfig;
#[cfg(feature = "swap")]
use vertex_swarm_node::args::SwapConfig;

#[cfg(feature = "chain")]
use crate::chain::SharedChainProvider;

type PeerStore = Arc<dyn PeerSnapshotStore<PeerSnapshot>>;

/// Stats collection interval for database metrics.
const DB_METRICS_INTERVAL: Duration = Duration::from_secs(30);

fn log_build_start<N>(node_type: SwarmNodeType, spec: &Spec, network: &N)
where
    N: SwarmPeerConfig,
    N::Peers: PeerConfigValues,
{
    info!(%node_type, "Building node...");
    spec.log();

    match network.peers().store_path() {
        Some(path) => info!(path = %path.display(), "Peers database"),
        None => info!("Peers database: ephemeral"),
    }
}

#[cfg(not(feature = "swap"))]
#[allow(clippy::type_complexity)]
fn build_accounting<A>(
    spec: Arc<Spec>,
    identity: &Arc<Identity>,
    config: A,
) -> ClientAccounting<
    Arc<vertex_swarm_bandwidth::Accounting<A, Arc<Identity>>>,
    <A::Pricing as vertex_swarm_api::SwarmPricingBuilder<Spec>>::Pricer,
>
where
    A: vertex_swarm_api::SwarmAccountingConfig
        + vertex_swarm_api::SwarmPricingConfig
        + Clone
        + 'static,
    A::Pricing: vertex_swarm_api::SwarmPricingBuilder<Spec>,
{
    AccountingBuilder::new(config)
        .with_pricer_from_config(spec)
        .build(identity)
}

/// Build client accounting, embedding the SWAP settlement provider when enabled.
///
/// Returns the accounting plus the swap wiring to spawn after the node command
/// channel exists. When SWAP is not enabled the wiring is `None` and the
/// accounting is identical to [`build_accounting`].
#[cfg(feature = "swap")]
#[allow(clippy::type_complexity)]
fn build_accounting_with_swap(
    spec: &Arc<Spec>,
    identity: &Arc<Identity>,
    config: &DefaultBandwidthConfig,
    swap_config: &SwapConfig,
) -> (
    ClientAccounting<
        Arc<vertex_swarm_bandwidth::Accounting<DefaultBandwidthConfig, Arc<Identity>>>,
        FixedPricer<Spec>,
    >,
    Option<crate::swap::SwapWiring>,
) {
    let builder = AccountingBuilder::new(config.clone()).with_pricer_from_config(Arc::clone(spec));

    match crate::swap::SwapWiring::prepare(spec, identity, config, swap_config) {
        Some((provider, wiring)) => (
            builder.with_settlement(provider).build(identity),
            Some(wiring),
        ),
        None => (builder.build(identity), None),
    }
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
/// Builds the bandwidth accounting (with SWAP settlement embedded when enabled),
/// constructs the node and the verified chunk provider, spawns the client
/// service and the SWAP settlement service, connects the chain provider when the
/// node type requires one, and wraps the node run loop in a task that owns the
/// accounting and chain handles for the node's lifetime.
async fn build_client_backed_node(
    ctx: &dyn InfrastructureContext,
    params: ClientNodeParams<'_>,
) -> Result<ClientNodeParts, SwarmNodeError> {
    let node_type = params.node_type;
    log_build_start(node_type, params.spec, params.network);

    let db = open_shared_database(ctx);
    let peer_store = create_peer_store(&db);

    // Prepare SWAP settlement first: the provider must be embedded in the
    // accounting, and the swap event sink must be routed at node build time.
    #[cfg(feature = "swap")]
    let (accounting, swap_wiring) =
        build_accounting_with_swap(params.spec, params.identity, params.bandwidth, params.swap);
    #[cfg(not(feature = "swap"))]
    let accounting = build_accounting(
        params.spec.clone(),
        params.identity,
        params.bandwidth.clone(),
    );

    let node_builder = ClientNode::builder(params.identity.clone());
    #[cfg(feature = "swap")]
    let node_builder = match swap_wiring.as_ref() {
        Some(wiring) => node_builder.with_swap_events(wiring.swap_event_sender()),
        None => node_builder,
    };
    let (node, client_service, client_handle) = node_builder
        .build(params.network, peer_store)
        .await
        .map_err(|e| SwarmNodeError::Build(e.into()))?;

    let topology = node.topology_handle().clone();
    spawn_peer_manager_task(
        Arc::clone(topology.peer_manager()),
        DEFAULT_TICK_INTERVAL,
        ctx.executor(),
    );
    let chunk_provider = NetworkChunkProvider::new(client_handle.clone(), topology.clone());
    let chunks = VerifyingChunkProvider::new(chunk_provider, params.verify);

    // Spawn client service as independent task with graceful shutdown
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
        log_build_start(SwarmNodeType::Bootnode, self.spec(), self.network());

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
