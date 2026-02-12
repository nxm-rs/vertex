//! Layered node builders for Swarm nodes.
//!
//! Provides fluent builder APIs for constructing nodes. The actual build logic
//! lives in SwarmLaunchConfig implementations in launch.rs.

use std::sync::Arc;

use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{
    SwarmAccountingConfig, SwarmIdentity, SwarmLaunchConfig, SwarmLocalStoreConfig,
    SwarmNetworkConfig, SwarmPeerConfig, SwarmPricingConfig, SwarmRoutingConfig,
    SwarmStorageConfig,
};
use vertex_swarm_bandwidth::DefaultBandwidthConfig;
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_redistribution::StorageConfig;
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::KademliaConfig;

use crate::builder_ext::{BuilderExt, WithInfrastructure};
use crate::config::{BootnodeConfig, ClientConfig, StorerConfig};
use crate::error::SwarmNodeError;
use crate::handle::{BootnodeHandle, ClientHandle, NodeHandle, StorerHandle};

/// Builder for bootnodes.
pub struct NodeBuilder<I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
    spec: Arc<Spec>,
    identity: I,
    network: N,
}

impl<I, N> BuilderExt for NodeBuilder<I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
}

impl<I, N> NodeBuilder<I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
    pub fn new(spec: Arc<Spec>, identity: I, network: N) -> Self {
        Self { spec, identity, network }
    }

    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }

    pub fn identity(&self) -> &I {
        &self.identity
    }

    pub fn network(&self) -> &N {
        &self.network
    }

    /// Transition to client builder by adding accounting.
    pub fn with_accounting<A>(self, accounting: A) -> ClientNodeBuilder<I, N, A>
    where
        A: SwarmAccountingConfig + SwarmPricingConfig,
    {
        ClientNodeBuilder { base: self, accounting }
    }
}

impl<I, R: Default> WithInfrastructure<NetworkConfig<R>> for NodeBuilder<I, NetworkConfig<R>>
where
    I: SwarmIdentity,
{
    fn network_mut(&mut self) -> &mut NetworkConfig<R> {
        &mut self.network
    }
}

/// Builder for client nodes.
pub struct ClientNodeBuilder<I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
{
    base: NodeBuilder<I, N>,
    accounting: A,
}

impl<I, N, A> BuilderExt for ClientNodeBuilder<I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
{
}

impl<I, N, A> ClientNodeBuilder<I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
{
    pub fn spec(&self) -> &Arc<Spec> {
        self.base.spec()
    }

    /// Transition to storer builder by adding storage.
    pub fn with_storage<S, St>(self, local_store: S, storage: St) -> StorerNodeBuilder<I, N, A, S, St>
    where
        S: SwarmLocalStoreConfig,
        St: SwarmStorageConfig,
    {
        StorerNodeBuilder { client: self, local_store, storage }
    }
}

impl<I, R: Default, A> WithInfrastructure<NetworkConfig<R>>
    for ClientNodeBuilder<I, NetworkConfig<R>, A>
where
    I: SwarmIdentity,
    A: SwarmAccountingConfig + SwarmPricingConfig,
{
    fn network_mut(&mut self) -> &mut NetworkConfig<R> {
        &mut self.base.network
    }
}

/// Builder for storer nodes.
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

impl<I, N, A, S, St> BuilderExt for StorerNodeBuilder<I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
}

impl<I, N, A, S, St> StorerNodeBuilder<I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    pub fn spec(&self) -> &Arc<Spec> {
        self.client.spec()
    }
}

impl<I, R: Default, A, S, St> WithInfrastructure<NetworkConfig<R>>
    for StorerNodeBuilder<I, NetworkConfig<R>, A, S, St>
where
    I: SwarmIdentity,
    A: SwarmAccountingConfig + SwarmPricingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    fn network_mut(&mut self) -> &mut NetworkConfig<R> {
        &mut self.client.base.network
    }
}

/// Default bootnode builder.
pub type DefaultNodeBuilder = NodeBuilder<Arc<Identity>, NetworkConfig<KademliaConfig>>;

/// Default client builder.
pub type DefaultClientBuilder =
    ClientNodeBuilder<Arc<Identity>, NetworkConfig<KademliaConfig>, DefaultBandwidthConfig>;

/// Default storer builder.
pub type DefaultStorerBuilder = StorerNodeBuilder<
    Arc<Identity>,
    NetworkConfig<KademliaConfig>,
    DefaultBandwidthConfig,
    LocalStoreConfig,
    StorageConfig,
>;

impl DefaultNodeBuilder {
    pub fn from_config(config: BootnodeConfig) -> Self {
        Self::new(config.spec().clone(), config.identity().clone(), config.network().clone())
    }

    /// Convert to config for building.
    pub fn into_config(self) -> BootnodeConfig {
        BootnodeConfig::new(self.spec, self.identity, self.network)
    }

    /// Build the bootnode. Delegates to SwarmLaunchConfig::build().
    pub async fn build(self, ctx: &dyn InfrastructureContext) -> Result<BootnodeHandle, SwarmNodeError> {
        let config = self.into_config();
        let (task, providers) = config.build(ctx).await?;
        Ok(NodeHandle::new(task, providers))
    }
}

impl DefaultClientBuilder {
    pub fn from_parts(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        bandwidth: DefaultBandwidthConfig,
    ) -> Self {
        NodeBuilder::new(spec, identity, network).with_accounting(bandwidth)
    }

    pub fn from_config(config: ClientConfig) -> Self {
        Self::from_parts(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
            config.bandwidth().clone(),
        )
    }

    /// Convert to config for building.
    pub fn into_config(self) -> ClientConfig {
        ClientConfig::new(self.base.spec, self.base.identity, self.base.network, self.accounting)
    }

    /// Build the client node. Delegates to SwarmLaunchConfig::build().
    pub async fn build(self, ctx: &dyn InfrastructureContext) -> Result<ClientHandle, SwarmNodeError> {
        let config = self.into_config();
        let (task, providers) = config.build(ctx).await?;
        Ok(NodeHandle::new(task, providers))
    }
}

impl DefaultStorerBuilder {
    pub fn from_parts(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        bandwidth: DefaultBandwidthConfig,
        local_store: LocalStoreConfig,
        storage: StorageConfig,
    ) -> Self {
        NodeBuilder::new(spec, identity, network)
            .with_accounting(bandwidth)
            .with_storage(local_store, storage)
    }

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

    /// Convert to config for building.
    pub fn into_config(self) -> StorerConfig {
        StorerConfig::new(
            self.client.base.spec,
            self.client.base.identity,
            self.client.base.network,
            self.client.accounting,
            self.local_store,
            self.storage,
        )
    }

    /// Build the storer node. Delegates to SwarmLaunchConfig::build().
    pub async fn build(self, ctx: &dyn InfrastructureContext) -> Result<StorerHandle, SwarmNodeError> {
        let config = self.into_config();
        let (task, providers) = config.build(ctx).await?;
        Ok(NodeHandle::new(task, providers))
    }
}

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
