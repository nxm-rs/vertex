//! Layered node builders for Swarm nodes.
//!
//! Provides fluent builder APIs for constructing nodes. The actual build logic
//! lives in SwarmLaunchConfig implementations in launch.rs.

use std::sync::Arc;

use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_api::{
    ReserveStore, SwarmAccountingConfig, SwarmIdentity, SwarmLaunchConfig, SwarmLocalStore,
    SwarmLocalStoreConfig, SwarmNetworkConfig, SwarmPeerConfig, SwarmPricingConfig,
    SwarmRoutingConfig, SwarmStorageConfig,
};
use vertex_swarm_bandwidth::DefaultBandwidthConfig;
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_redistribution::StorageConfig;
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::KademliaConfig;

use crate::config::{BootnodeConfig, ClientConfig, StorerConfig};
use crate::error::SwarmNodeError;
use crate::handle::{BuiltBootnode, BuiltClient, BuiltNode, BuiltStorer};
use crate::launch::{CacheSeam, ReserveSeam};
use crate::verify::ChunkVerifyConfig;

/// Fluent transformation API for builders.
pub trait BuilderExt: Sized {
    fn apply<F>(self, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        f(self)
    }

    fn apply_if<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond { f(self) } else { self }
    }
}

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
        Self {
            spec,
            identity,
            network,
        }
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
        ClientNodeBuilder {
            base: self,
            accounting,
            local_store: LocalStoreConfig::default(),
            verify: ChunkVerifyConfig::default(),
            chain: ChainConfig::default(),
            swap: SwapConfig::default(),
            cache: None,
            reserve: None,
        }
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
    local_store: LocalStoreConfig,
    verify: ChunkVerifyConfig,
    chain: ChainConfig,
    swap: SwapConfig,
    /// `None` builds the default in-memory cache sized from `local_store`.
    cache: Option<CacheSeam>,
    /// Carried here so a wrapping storer builder keeps it across the storage
    /// transition; consumed only by the storer build path.
    reserve: Option<ReserveSeam>,
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

    /// Set the verification checks applied to downloaded chunks.
    pub fn with_verify(mut self, verify: ChunkVerifyConfig) -> Self {
        self.verify = verify;
        self
    }

    /// Set the chain configuration (RPC endpoint and transaction tuning).
    pub fn with_chain(mut self, chain: ChainConfig) -> Self {
        self.chain = chain;
        self
    }

    /// Set the SWAP settlement configuration (chequebook, beneficiary, deploy).
    pub fn with_swap(mut self, swap: SwapConfig) -> Self {
        self.swap = swap;
        self
    }

    /// Cache sizing for the default in-memory cache; ignored when a cache seam
    /// is set.
    pub fn with_local_store(mut self, local_store: LocalStoreConfig) -> Self {
        self.local_store = local_store;
        self
    }

    /// Override the cache with a pre-built local store, ignoring the shared
    /// database handle.
    pub fn with_cache(mut self, cache: Arc<dyn SwarmLocalStore>) -> Self {
        self.cache = Some(CacheSeam::Ready(cache));
        self
    }

    /// Override the cache with a factory invoked at build time with the opened
    /// shared database (`None` in-memory).
    pub fn with_cache_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.cache = Some(CacheSeam::Factory(Box::new(factory)));
        self
    }

    /// Override the storer reserve with a pre-built store; consumed only by the
    /// storer build path.
    pub fn with_reserve(mut self, reserve: Arc<dyn ReserveStore>) -> Self {
        self.reserve = Some(ReserveSeam::Ready(reserve));
        self
    }

    /// Override the storer reserve with a factory invoked at build time with the
    /// opened shared database (`None` in-memory).
    pub fn with_reserve_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn ReserveStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.reserve = Some(ReserveSeam::Factory(Box::new(factory)));
        self
    }

    /// Transition to storer builder by adding storage.
    pub fn with_storage<S, St>(
        self,
        local_store: S,
        storage: St,
    ) -> StorerNodeBuilder<I, N, A, S, St>
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

    /// Override the cache with a pre-built local store. See
    /// [`ClientNodeBuilder::with_cache`].
    pub fn with_cache(mut self, cache: Arc<dyn SwarmLocalStore>) -> Self {
        self.client = self.client.with_cache(cache);
        self
    }

    /// Override the cache with a factory. See
    /// [`ClientNodeBuilder::with_cache_factory`].
    pub fn with_cache_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.client = self.client.with_cache_factory(factory);
        self
    }

    /// Override the reserve with a pre-built store. See
    /// [`ClientNodeBuilder::with_reserve`].
    pub fn with_reserve(mut self, reserve: Arc<dyn ReserveStore>) -> Self {
        self.client = self.client.with_reserve(reserve);
        self
    }

    /// Override the reserve with a factory. See
    /// [`ClientNodeBuilder::with_reserve_factory`].
    pub fn with_reserve_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn ReserveStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.client = self.client.with_reserve_factory(factory);
        self
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
        Self::new(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
        )
    }

    /// Convert to config for building.
    pub fn into_config(self) -> BootnodeConfig {
        BootnodeConfig::new(self.spec, self.identity, self.network)
    }

    /// Build the bootnode. Delegates to SwarmLaunchConfig::build().
    pub async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<BuiltBootnode, SwarmNodeError> {
        let config = self.into_config();
        let (task, providers) = config.build(ctx).await?;
        Ok(BuiltNode::new(task, providers))
    }
}

impl DefaultClientBuilder {
    pub fn from_parts(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        bandwidth: DefaultBandwidthConfig,
        verify: ChunkVerifyConfig,
    ) -> Self {
        NodeBuilder::new(spec, identity, network)
            .with_accounting(bandwidth)
            .with_verify(verify)
    }

    pub fn from_config(config: ClientConfig) -> Self {
        Self::from_parts(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
            config.bandwidth().clone(),
            config.verify(),
        )
        .with_local_store(config.local_store().clone())
        .with_chain(config.chain().clone())
        .with_swap(config.swap().clone())
    }

    /// Convert to config for building. Drops any store seam, since [`ClientConfig`]
    /// is `Clone`; prefer [`build`](Self::build), which consumes the seam directly.
    pub fn into_config(self) -> ClientConfig {
        ClientConfig::new(
            self.base.spec,
            self.base.identity,
            self.base.network,
            self.accounting,
            self.local_store,
            self.verify,
            self.chain,
            self.swap,
        )
    }

    /// Build the client node, honoring any cache seam set on the builder.
    pub async fn build(
        mut self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<BuiltClient, SwarmNodeError> {
        let cache = self.cache.take();
        let config = self.into_config();
        let (task, providers) = crate::launch::build_client(config, ctx, cache).await?;
        Ok(BuiltNode::new(task, providers))
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
        verify: ChunkVerifyConfig,
    ) -> Self {
        NodeBuilder::new(spec, identity, network)
            .with_accounting(bandwidth)
            .with_verify(verify)
            .with_storage(local_store, storage)
    }

    pub fn from_config(config: StorerConfig) -> Self {
        let chain = config.chain().clone();
        let swap = config.swap().clone();
        let mut builder = Self::from_parts(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
            config.bandwidth().clone(),
            config.local_store().clone(),
            config.storage().clone(),
            config.verify(),
        );
        builder.client = builder.client.with_chain(chain).with_swap(swap);
        builder
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
            self.client.verify,
            self.client.chain,
            self.client.swap,
        )
    }

    /// Build the storer node, honoring any cache or reserve seam set on the
    /// builder.
    pub async fn build(
        mut self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<BuiltStorer, SwarmNodeError> {
        let cache = self.client.cache.take();
        let reserve = self.client.reserve.take();
        let config = self.into_config();
        let (task, providers) = crate::launch::build_storer(config, ctx, cache, reserve).await?;
        Ok(BuiltNode::new(task, providers))
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
