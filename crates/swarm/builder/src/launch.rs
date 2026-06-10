//! SwarmLaunchConfig implementations for config types.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use vertex_net_peer_store::NetPeerStore;
use vertex_net_peer_store::error::StoreError;
use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
#[cfg(feature = "swap")]
use vertex_swarm_api::SwarmClientAccounting;
use vertex_swarm_api::{PeerConfigValues, SwarmLaunchConfig, SwarmPeerConfig, SwarmScoreStore};
use vertex_swarm_bandwidth::{
    Accounting, AccountingBuilder, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
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
use crate::verify::VerifyingChunkProvider;

/// Network chunk provider wrapped with config-gated download verification.
type VerifiedChunkProvider = VerifyingChunkProvider<NetworkChunkProvider<Arc<Identity>>>;

#[cfg(feature = "chain")]
use vertex_swarm_api::{SwarmAccountingConfig, SwarmSpec};
#[cfg(feature = "chain")]
use vertex_swarm_node::SwarmNodeType;
#[cfg(feature = "chain")]
use vertex_swarm_node::args::ChainConfig;

#[cfg(feature = "chain")]
use crate::chain::SharedChainProvider;

type PeerStore = Arc<dyn NetPeerStore<StoredPeer>>;
type PeerScoreStore = Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>;

/// Stats collection interval for database metrics.
const DB_METRICS_INTERVAL: Duration = Duration::from_secs(30);

fn log_build_start<N>(node_type: &str, spec: &Spec, network: &N)
where
    N: SwarmPeerConfig,
    N::Peers: PeerConfigValues,
{
    info!("Building {} node...", node_type);
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
    swap_config: &vertex_swarm_node::args::SwapConfig,
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
            type Accounting = ClientAccounting<
                Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>,
                FixedPricer<Arc<Spec>>,
            >;
        }
    };
}

define_launch_types!(BootnodeLaunchTypes);
define_launch_types!(ClientLaunchTypes, with_client);
define_launch_types!(StorerLaunchTypes, with_client);

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

impl SwarmLaunchConfig for ClientConfig {
    type Types = ClientLaunchTypes;
    type Providers = ClientRpcProviders<Arc<Identity>, VerifiedChunkProvider>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start("Client", self.spec(), self.network());

        let db = open_shared_database(ctx);
        let (peer_store, score_store) = create_peer_store(&db);

        // Prepare SWAP settlement first: the provider must be embedded in the
        // accounting, and the swap event sink must be routed at node build time.
        #[cfg(feature = "swap")]
        let (accounting, swap_wiring) =
            build_accounting_with_swap(self.spec(), self.identity(), self.bandwidth(), self.swap());
        #[cfg(not(feature = "swap"))]
        let accounting = build_accounting(
            self.spec().clone(),
            self.identity(),
            self.bandwidth().clone(),
        );

        let node_builder = ClientNode::builder(self.identity().clone());
        #[cfg(feature = "swap")]
        let node_builder = match swap_wiring.as_ref() {
            Some(wiring) => node_builder.with_swap_events(wiring.swap_event_sender()),
            None => node_builder,
        };
        let (node, client_service, client_handle) = node_builder
            .build(self.network(), peer_store, score_store)
            .await
            .map_err(|e| SwarmNodeError::Build(e.into()))?;

        let topology = node.topology_handle().clone();
        let chunk_provider = NetworkChunkProvider::new(client_handle.clone(), topology.clone());
        let verified_provider = VerifyingChunkProvider::new(chunk_provider, self.verify());
        let providers = ClientRpcProviders::new(topology, verified_provider);

        // Spawn client service as independent task with graceful shutdown
        ctx.executor()
            .spawn_service("swarm.client_service", client_service);

        // A client needs a chain only when SWAP settlement is enabled.
        #[cfg(feature = "chain")]
        let chain_provider = {
            let swap_enabled = SwarmAccountingConfig::mode(self.bandwidth()).swap_enabled();
            build_node_chain_provider(
                self.spec(),
                self.identity(),
                SwarmNodeType::Client,
                swap_enabled,
                self.chain(),
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
                tracing::error!(error = %e, "ClientNode error");
            }
        });

        info!("Client node built successfully");
        Ok((task, providers))
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
        log_build_start("Storer", self.spec(), self.network());

        let db = open_shared_database(ctx);
        let (peer_store, score_store) = create_peer_store(&db);

        // Prepare SWAP settlement first: the provider must be embedded in the
        // accounting, and the swap event sink must be routed at node build time.
        #[cfg(feature = "swap")]
        let (accounting, swap_wiring) =
            build_accounting_with_swap(self.spec(), self.identity(), self.bandwidth(), self.swap());
        #[cfg(not(feature = "swap"))]
        let accounting = build_accounting(
            self.spec().clone(),
            self.identity(),
            self.bandwidth().clone(),
        );

        // TODO: build storer-specific components
        let _ = self.local_store();
        let _ = self.storage();

        // Build as ClientNode for now (storer components not yet implemented)
        let node_builder = ClientNode::builder(self.identity().clone());
        #[cfg(feature = "swap")]
        let node_builder = match swap_wiring.as_ref() {
            Some(wiring) => node_builder.with_swap_events(wiring.swap_event_sender()),
            None => node_builder,
        };
        let (node, client_service, client_handle) = node_builder
            .build(self.network(), peer_store, score_store)
            .await
            .map_err(|e| SwarmNodeError::Build(e.into()))?;

        let topology = node.topology_handle().clone();
        let chunk_provider = NetworkChunkProvider::new(client_handle.clone(), topology.clone());
        let verified_provider = VerifyingChunkProvider::new(chunk_provider, self.verify());
        let providers = StorerRpcProviders::new(topology, verified_provider);

        // Spawn client service as independent task with graceful shutdown
        ctx.executor()
            .spawn_service("swarm.client_service", client_service);

        // A storer always needs a chain (staking, oracle, settlement).
        #[cfg(feature = "chain")]
        let chain_provider = {
            let swap_enabled = SwarmAccountingConfig::mode(self.bandwidth()).swap_enabled();
            build_node_chain_provider(
                self.spec(),
                self.identity(),
                SwarmNodeType::Storer,
                swap_enabled,
                self.chain(),
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
                tracing::error!(error = %e, "StorerNode error");
            }
        });

        info!("Storer node built successfully");
        Ok((task, providers))
    }
}
