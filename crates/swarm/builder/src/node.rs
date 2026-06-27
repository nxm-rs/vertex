//! Layered node builders for Swarm nodes.
//!
//! Provides fluent builder APIs for constructing nodes. The actual build logic
//! lives in SwarmLaunchConfig implementations in launch.rs.

use std::sync::Arc;

use vertex_node_api::InfrastructureContext;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_accounting::DefaultBandwidthConfig;
use vertex_swarm_api::{
    SwarmAccountingConfig, SwarmIdentity, SwarmLaunchConfig, SwarmLocalStore, SwarmNetworkConfig,
    SwarmPeerConfig, SwarmPricingConfig, SwarmRoutingConfig,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::KademliaConfig;

use crate::config::{BootnodeConfig, ClientConfig};
use crate::error::SwarmNodeError;
use crate::handle::{BuiltBootnode, BuiltClient, BuiltNode};
use crate::launch::CacheSeam;
use crate::verify::ChunkVerifyConfig;

/// Builder for bootnodes.
pub struct NodeBuilder<I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
    pub(crate) spec: Arc<Spec>,
    pub(crate) identity: I,
    pub(crate) network: N,
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
    pub(crate) base: NodeBuilder<I, N>,
    pub(crate) accounting: A,
    pub(crate) local_store: LocalStoreConfig,
    pub(crate) verify: ChunkVerifyConfig,
    pub(crate) chain: ChainConfig,
    pub(crate) swap: SwapConfig,
    /// `None` builds the default in-memory cache sized from `local_store`.
    pub(crate) cache: Option<CacheSeam>,
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

    /// Override the cache with a pre-built local store.
    ///
    /// Replaces the default in-memory [`ChunkStore`]. The opened shared database
    /// handle is not offered; use [`with_cache_factory`](Self::with_cache_factory)
    /// to back the cache onto it.
    ///
    /// On a client this is the full retrieval-serve view; on a storer only the
    /// forwarding-cache layer of the cache-then-reserve view (read after the reserve,
    /// written for out-of-AoR chunks, never for reserve admission).
    pub fn with_cache(mut self, cache: Arc<dyn SwarmLocalStore>) -> Self {
        self.cache = Some(CacheSeam::Ready(cache));
        self
    }

    /// Override the cache with a factory invoked at build time.
    ///
    /// The factory receives the opened shared database (`None` in-memory) so the
    /// cache can size or back itself from the same handle the rest of the node
    /// uses. The client-versus-storer seam meaning of [`with_cache`](Self::with_cache)
    /// applies: on a storer the built store is only the forwarding-cache layer.
    pub fn with_cache_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.cache = Some(CacheSeam::Factory(Box::new(factory)));
        self
    }
}

/// Default bootnode builder.
pub type DefaultNodeBuilder = NodeBuilder<Arc<Identity>, NetworkConfig<KademliaConfig>>;

/// Default client builder.
pub type DefaultClientBuilder =
    ClientNodeBuilder<Arc<Identity>, NetworkConfig<KademliaConfig>, DefaultBandwidthConfig>;

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
