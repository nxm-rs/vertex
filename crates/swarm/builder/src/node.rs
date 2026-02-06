//! Layered node builders: NodeBuilder → ClientNodeBuilder → StorerNodeBuilder.
//!
//! Builders are generic over identity and configuration types, with trait bounds
//! ensuring type safety. Each layer adds required capabilities.

use std::sync::Arc;

use vertex_swarm_api::{
    NodeTask, PeerConfigValues, SwarmAccountingConfig, SwarmIdentity, SwarmLocalStoreConfig,
    SwarmNetworkConfig, SwarmPeerConfig, SwarmPricingBuilder, SwarmPricingConfig, SwarmRoutingConfig,
    SwarmStorageConfig,
};
use vertex_swarm_bandwidth::{AccountingBuilder, ClientAccounting, DefaultBandwidthConfig};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_node::ClientNode;
use vertex_swarm_redistribution::StorageConfig;
use vertex_swarm_spec::{Loggable, Spec};
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::SpawnableTask;

use crate::config::{BootnodeConfig, ClientConfig, StorerConfig};
use crate::error::SwarmNodeError;
use crate::providers::NetworkChunkProvider;
use crate::rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

/// Handle returned from launching a base node (bootnode).
pub struct BaseNodeHandle<I: SwarmIdentity> {
    task: NodeTask,
    rpc_providers: BootnodeRpcProviders<I>,
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> BaseNodeHandle<I> {
    /// Consume and return the main event loop task.
    pub fn task(self) -> NodeTask {
        self.task
    }

    /// Get the RPC providers.
    pub fn rpc_providers(&self) -> &BootnodeRpcProviders<I> {
        &self.rpc_providers
    }

    /// Get the topology handle.
    pub fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }

    /// Decompose into parts.
    pub fn into_parts(self) -> (NodeTask, BootnodeRpcProviders<I>, TopologyHandle<I>) {
        (self.task, self.rpc_providers, self.topology)
    }
}

/// Handle returned from launching a client node.
pub struct ClientNodeHandle<I: SwarmIdentity> {
    task: NodeTask,
    rpc_providers: ClientRpcProviders<I, NetworkChunkProvider<I>>,
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> ClientNodeHandle<I> {
    /// Consume and return the main event loop task.
    pub fn task(self) -> NodeTask {
        self.task
    }

    /// Get the RPC providers.
    pub fn rpc_providers(&self) -> &ClientRpcProviders<I, NetworkChunkProvider<I>> {
        &self.rpc_providers
    }

    /// Get the topology handle.
    pub fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }

    /// Decompose into parts.
    pub fn into_parts(
        self,
    ) -> (
        NodeTask,
        ClientRpcProviders<I, NetworkChunkProvider<I>>,
        TopologyHandle<I>,
    ) {
        (self.task, self.rpc_providers, self.topology)
    }
}

/// Handle returned from launching a storer node.
pub struct StorerNodeHandle<I: SwarmIdentity> {
    task: NodeTask,
    rpc_providers: StorerRpcProviders<I>,
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> StorerNodeHandle<I> {
    /// Consume and return the main event loop task.
    pub fn task(self) -> NodeTask {
        self.task
    }

    /// Get the RPC providers.
    pub fn rpc_providers(&self) -> &StorerRpcProviders<I> {
        &self.rpc_providers
    }

    /// Get the topology handle.
    pub fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }

    /// Decompose into parts.
    pub fn into_parts(self) -> (NodeTask, StorerRpcProviders<I>, TopologyHandle<I>) {
        (self.task, self.rpc_providers, self.topology)
    }
}

/// Builder for base nodes (bootnodes).
///
/// Generic over identity type `I` and network configuration type `N`.
/// The network's routing config must implement `SwarmTopologyBuilder<I>`
/// to enable topology construction.
pub struct NodeBuilder<I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
    spec: Arc<Spec>,
    identity: I,
    network: N,
}

impl<I, N> NodeBuilder<I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
    /// Create a new node builder.
    pub fn new(spec: Arc<Spec>, identity: I, network: N) -> Self {
        Self {
            spec,
            identity,
            network,
        }
    }

    /// Apply a transformation function.
    pub fn apply<F>(self, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        f(self)
    }

    /// Apply a transformation function if condition is true.
    pub fn apply_if<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond { f(self) } else { self }
    }

    /// Get a reference to the spec.
    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }

    /// Get a reference to the identity.
    pub fn identity(&self) -> &I {
        &self.identity
    }

    /// Get a reference to the network config.
    pub fn network(&self) -> &N {
        &self.network
    }

    /// Transition to a client builder with accounting configuration.
    pub fn with_accounting<A>(self, accounting: A) -> ClientNodeBuilder<I, N, A>
    where
        A: SwarmAccountingConfig + SwarmPricingConfig,
    {
        ClientNodeBuilder {
            base: self,
            accounting,
        }
    }
}

impl<I, N> NodeBuilder<I, N>
where
    I: SwarmIdentity + Clone,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
{
    /// Build and launch a base node (bootnode).
    pub async fn build(self) -> Result<BaseNodeHandle<I>, SwarmNodeError> {
        use tracing::info;

        info!("Building Bootnode...");
        self.spec.log();
        log_peers_path(&self.network);

        let node = vertex_swarm_node::BootNode::builder(self.identity.clone())
            .build(&self.network)
            .await
            .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        let topology = node.topology_handle().clone();
        let rpc_providers = BootnodeRpcProviders::new(topology.clone());

        let task: NodeTask = Box::pin(async move {
            node.into_task().await;
        });

        info!("Bootnode built successfully");
        Ok(BaseNodeHandle {
            task,
            rpc_providers,
            topology,
        })
    }
}

/// Builder for client nodes.
///
/// Generic over identity `I`, network config `N`, and accounting config `A`.
/// Extends [`NodeBuilder`] with bandwidth accounting.
pub struct ClientNodeBuilder<I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
{
    base: NodeBuilder<I, N>,
    accounting: A,
}

impl<I, N, A> ClientNodeBuilder<I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
{
    /// Apply a transformation function.
    pub fn apply<F>(self, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        f(self)
    }

    /// Apply a transformation function if condition is true.
    pub fn apply_if<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond { f(self) } else { self }
    }

    /// Get a reference to the spec.
    pub fn spec(&self) -> &Arc<Spec> {
        self.base.spec()
    }

    /// Get a reference to the identity.
    pub fn identity(&self) -> &I {
        self.base.identity()
    }

    /// Transition to a storer builder with storage configurations.
    pub fn with_storage<S, St>(self, local_store: S, storage: St) -> StorerNodeBuilder<I, N, A, S, St>
    where
        S: SwarmLocalStoreConfig,
        St: SwarmStorageConfig,
    {
        StorerNodeBuilder {
            client: self,
            local_store,
            storage,
        }
    }
}

impl<I, N, A> ClientNodeBuilder<I, N, A>
where
    I: SwarmIdentity + Clone,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    A: SwarmAccountingConfig + SwarmPricingConfig + Clone + 'static,
    A::Pricing: SwarmPricingBuilder<Spec>,
{
    /// Build and launch a client node.
    pub async fn build(self) -> Result<ClientNodeHandle<I>, SwarmNodeError> {
        use tracing::info;

        let NodeBuilder {
            spec,
            identity,
            network,
        } = self.base;

        info!("Building Client node...");
        spec.log();
        log_peers_path(&network);

        // Build accounting with pricer from config
        // TODO: Wire accounting into the node once ClientNode supports it
        let _accounting: ClientAccounting<_, _> = AccountingBuilder::new(self.accounting.clone())
            .with_pricer_from_config(spec.clone())
            .build(&identity);

        let (node, client_service, client_handle) = ClientNode::builder(identity.clone())
            .build(&network)
            .await
            .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        let topology = node.topology_handle().clone();
        let chunk_provider = NetworkChunkProvider::new(client_handle.clone(), topology.clone());
        let rpc_providers = ClientRpcProviders::new(topology.clone(), chunk_provider);

        let task: NodeTask = Box::pin(async move {
            tokio::select! {
                _ = node.into_task() => {
                    tracing::info!("Node task completed");
                }
                _ = client_service.run() => {
                    tracing::info!("Client service completed");
                }
            }
        });

        info!("Client node built successfully");
        Ok(ClientNodeHandle {
            task,
            rpc_providers,
            topology,
        })
    }
}

/// Builder for storer (full) nodes.
///
/// Generic over identity `I`, network config `N`, accounting config `A`,
/// local store config `S`, and storage config `St`.
/// Extends [`ClientNodeBuilder`] with local storage and redistribution.
pub struct StorerNodeBuilder<I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    client: ClientNodeBuilder<I, N, A>,
    local_store: S,
    storage: St,
}

impl<I, N, A, S, St> StorerNodeBuilder<I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    /// Apply a transformation function.
    pub fn apply<F>(self, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        f(self)
    }

    /// Apply a transformation function if condition is true.
    pub fn apply_if<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond { f(self) } else { self }
    }

    /// Get a reference to the spec.
    pub fn spec(&self) -> &Arc<Spec> {
        self.client.spec()
    }

    /// Get a reference to the identity.
    pub fn identity(&self) -> &I {
        self.client.identity()
    }
}

impl<I, N, A, S, St> StorerNodeBuilder<I, N, A, S, St>
where
    I: SwarmIdentity + Clone,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    A: SwarmAccountingConfig + SwarmPricingConfig + Clone + 'static,
    A::Pricing: SwarmPricingBuilder<Spec>,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    /// Build and launch a storer node.
    pub async fn build(self) -> Result<StorerNodeHandle<I>, SwarmNodeError> {
        use tracing::info;

        let ClientNodeBuilder {
            base:
                NodeBuilder {
                    spec,
                    identity,
                    network,
                },
            accounting,
        } = self.client;

        info!("Building Storer node...");
        spec.log();
        log_peers_path(&network);

        // Build accounting with pricer from config
        // TODO: Wire accounting into the node once StorerNode supports it
        let _accounting: ClientAccounting<_, _> = AccountingBuilder::new(accounting)
            .with_pricer_from_config(spec.clone())
            .build(&identity);

        // TODO: Build storer-specific components:
        // - LocalStore from self.local_store config
        // - ChunkSync service
        // - Redistribution service from self.storage config
        let _ = self.local_store;
        let _ = self.storage;

        // Build as ClientNode for now (storer components stubbed)
        let (node, client_service, _client_handle) = ClientNode::builder(identity.clone())
            .build(&network)
            .await
            .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        let topology = node.topology_handle().clone();
        let rpc_providers = StorerRpcProviders::new(topology.clone());

        let task: NodeTask = Box::pin(async move {
            tokio::select! {
                _ = node.into_task() => {
                    tracing::info!("Node task completed");
                }
                _ = client_service.run() => {
                    tracing::info!("Client service completed");
                }
            }
        });

        info!("Storer node built successfully");
        Ok(StorerNodeHandle {
            task,
            rpc_providers,
            topology,
        })
    }
}

// Type aliases for common configurations using default types

/// Default node builder using Arc<Identity> and NetworkConfig<KademliaConfig>.
pub type DefaultNodeBuilder = NodeBuilder<Arc<Identity>, NetworkConfig<KademliaConfig>>;

/// Default client builder using standard types.
pub type DefaultClientBuilder =
    ClientNodeBuilder<Arc<Identity>, NetworkConfig<KademliaConfig>, DefaultBandwidthConfig>;

/// Default storer builder using standard types.
pub type DefaultStorerBuilder = StorerNodeBuilder<
    Arc<Identity>,
    NetworkConfig<KademliaConfig>,
    DefaultBandwidthConfig,
    LocalStoreConfig,
    StorageConfig,
>;

// Convenience constructors for default types

impl DefaultNodeBuilder {
    /// Create from a bootnode config.
    pub fn from_config(config: BootnodeConfig) -> Self {
        Self::new(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
        )
    }
}

impl DefaultClientBuilder {
    /// Create a client builder directly using default types.
    pub fn from_parts(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig,
        bandwidth: DefaultBandwidthConfig,
    ) -> Self {
        NodeBuilder::new(spec, identity, network).with_accounting(bandwidth)
    }

    /// Create from a client config.
    pub fn from_config(config: ClientConfig) -> Self {
        Self::from_parts(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
            config.bandwidth().clone(),
        )
    }
}

impl DefaultStorerBuilder {
    /// Create a storer builder directly using default types.
    pub fn from_parts(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig,
        bandwidth: DefaultBandwidthConfig,
        local_store: LocalStoreConfig,
        storage: StorageConfig,
    ) -> Self {
        NodeBuilder::new(spec, identity, network)
            .with_accounting(bandwidth)
            .with_storage(local_store, storage)
    }

    /// Create from a storer config.
    pub fn from_config(config: StorerConfig) -> Self {
        Self::from_parts(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
            config.bandwidth().clone(),
            config.local_store().clone(),
            config.storage().clone(),
        )
    }
}

// From implementations for ergonomic config -> builder conversion

impl From<BootnodeConfig> for DefaultNodeBuilder {
    fn from(config: BootnodeConfig) -> Self {
        Self::from_config(config)
    }
}

impl From<ClientConfig> for DefaultClientBuilder {
    fn from(config: ClientConfig) -> Self {
        Self::from_config(config)
    }
}

impl From<StorerConfig> for DefaultStorerBuilder {
    fn from(config: StorerConfig) -> Self {
        Self::from_config(config)
    }
}

fn log_peers_path<N: SwarmPeerConfig>(network: &N)
where
    N::Peers: PeerConfigValues,
{
    use tracing::info;
    if let Some(ref path) = network.peers().store_path() {
        info!("Peers database: {}", path.display());
    } else {
        info!("Peers database: ephemeral (in-memory)");
    }
}
