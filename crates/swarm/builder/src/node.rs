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
            #[cfg(feature = "swap")]
            bandwidth_mode_override: None,
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
    /// Effective bandwidth mode override recorded by `with_swap` when a chequebook is supplied;
    /// `None` leaves the node-type-seeded mode untouched, `Some` is applied at build time through
    /// `BandwidthConfig::with_mode_override` and is effective only for the default builders.
    #[cfg(feature = "swap")]
    bandwidth_mode_override: Option<vertex_swarm_api::BandwidthMode>,
    /// Optional cache override. `None` builds the default in-memory cache sized
    /// from `local_store`. The reserve seam rides on the client builder so a
    /// `StorerNodeBuilder` (which wraps a `ClientNodeBuilder`) can carry it
    /// before the storage transition.
    cache: Option<CacheSeam>,
    /// Optional reserve override, consumed only by the storer build path.
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
    ///
    /// Under the `swap` feature, supplying a chequebook address folds the effective bandwidth mode
    /// toward swap-enabled (`Pseudosettle` becomes `Both`, disabled becomes `Swap`) via
    /// [`BandwidthMode::with_swap`](vertex_swarm_api::BandwidthMode::with_swap). Without a chequebook
    /// no settlement provider is wired, so the mode is left untouched rather than metering debt the
    /// node can never settle. This diverges from the CLI/config path, where SWAP is selected solely
    /// by `--bandwidth.mode`; the fold applies only to the default builders.
    pub fn with_swap(mut self, swap: SwapConfig) -> Self {
        #[cfg(feature = "swap")]
        // No chequebook means no settlement provider is wired, so do not commit the mode to swap.
        if swap.chequebook.is_some() {
            let base = self
                .bandwidth_mode_override
                .unwrap_or_else(|| SwarmAccountingConfig::mode(&self.accounting));
            self.bandwidth_mode_override = Some(base.with_swap());
        }
        self.swap = swap;
        self
    }

    /// Record the chequebook config without touching the effective bandwidth mode, used by the
    /// config round-trip where the CLI has already resolved the mode and SWAP stays selected solely
    /// by the bandwidth mode (unlike the fluent [`with_swap`](Self::with_swap) seam).
    fn set_swap(mut self, swap: SwapConfig) -> Self {
        self.swap = swap;
        self
    }

    /// Set the cache sizing for the default in-memory client cache.
    ///
    /// Ignored when a cache is supplied through [`with_cache`](Self::with_cache)
    /// or [`with_cache_factory`](Self::with_cache_factory).
    pub fn with_local_store(mut self, local_store: LocalStoreConfig) -> Self {
        self.local_store = local_store;
        self
    }

    /// Override the cache with a pre-built local store.
    ///
    /// Replaces the default in-memory [`ChunkStore`]. The opened shared database
    /// handle is not offered; use [`with_cache_factory`](Self::with_cache_factory)
    /// to back the cache onto it.
    pub fn with_cache(mut self, cache: Arc<dyn SwarmLocalStore>) -> Self {
        self.cache = Some(CacheSeam::Ready(cache));
        self
    }

    /// Override the cache with a factory invoked at build time.
    ///
    /// The factory receives the opened shared database (`None` in-memory) so the
    /// cache can size or back itself from the same handle the rest of the node
    /// uses.
    pub fn with_cache_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.cache = Some(CacheSeam::Factory(Box::new(factory)));
        self
    }

    /// Override the storer reserve with a pre-built store.
    ///
    /// Carried on the client builder so it survives the storage transition;
    /// consumed only by the storer build path. A client never wires a reserve.
    pub fn with_reserve(mut self, reserve: Arc<dyn ReserveStore>) -> Self {
        self.reserve = Some(ReserveSeam::Ready(reserve));
        self
    }

    /// Override the storer reserve with a factory invoked at build time.
    ///
    /// The factory receives the opened shared database (`None` in-memory).
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

    /// Set the chain configuration. See [`ClientNodeBuilder::with_chain`].
    pub fn with_chain(mut self, chain: ChainConfig) -> Self {
        self.client = self.client.with_chain(chain);
        self
    }

    /// Set the SWAP settlement configuration. See [`ClientNodeBuilder::with_swap`]. A storer runs
    /// swap by node-type default, so the mode fold is a no-op and this supplies the chequebook config.
    pub fn with_swap(mut self, swap: SwapConfig) -> Self {
        self.client = self.client.with_swap(swap);
        self
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
        .set_swap(config.swap().clone())
    }

    /// Convert to config for building.
    ///
    /// Drops any store seam: a [`ClientConfig`] is `Clone` and carries neither the
    /// `FnOnce` factory variants nor a pre-built `Ready` cache. [`build`](Self::build)
    /// consumes the seam directly, so prefer it over `into_config().build()`
    /// whenever any seam (cache or reserve, `Ready` or `Factory`) is set.
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
        #[cfg(feature = "swap")]
        {
            self.accounting = self
                .accounting
                .with_mode_override(self.bandwidth_mode_override);
        }
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
        builder.client = builder.client.with_chain(chain).set_swap(swap);
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
        #[cfg(feature = "swap")]
        {
            self.client.accounting = self
                .client
                .accounting
                .with_mode_override(self.client.bandwidth_mode_override);
        }
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

#[cfg(all(test, feature = "swap"))]
mod swap_tests {
    use vertex_swarm_api::{BandwidthMode, SwarmAccountingConfig, SwarmNodeType};
    use vertex_swarm_node::args::{NetworkArgs, SwapConfig};
    use vertex_swarm_test_utils::test_identity_arc;

    use super::*;

    fn test_network() -> NetworkConfig<KademliaConfig> {
        let args = NetworkArgs {
            port: 0,
            mdns: false,
            disable_discovery: true,
            ..Default::default()
        };
        NetworkConfig::try_from(&args).expect("test network args are valid")
    }

    fn client_builder(node_type: SwarmNodeType) -> DefaultClientBuilder {
        let identity = test_identity_arc();
        DefaultClientBuilder::from_parts(
            identity.spec().clone(),
            identity,
            test_network(),
            DefaultBandwidthConfig::for_node_type(node_type),
            ChunkVerifyConfig::default(),
        )
    }

    /// A swap config carrying a chequebook, the path that selects swap through the fluent seam.
    fn swap_with_chequebook() -> SwapConfig {
        SwapConfig {
            chequebook: Some(alloy_primitives::Address::repeat_byte(0x11)),
            ..SwapConfig::default()
        }
    }

    /// The effective mode the build path would apply: seeded config mode plus the recorded override.
    fn effective_mode(builder: &DefaultClientBuilder) -> BandwidthMode {
        SwarmAccountingConfig::mode(
            &builder
                .accounting
                .clone()
                .with_mode_override(builder.bandwidth_mode_override),
        )
    }

    #[test]
    fn with_swap_drives_a_client_to_both() {
        // Client is seeded Pseudosettle; the chequebook folds swap in, preserving the leg.
        let builder = client_builder(SwarmNodeType::Client).with_swap(swap_with_chequebook());
        assert_eq!(effective_mode(&builder), BandwidthMode::Both);
    }

    #[test]
    fn with_swap_enables_swap_from_a_disabled_mode() {
        let builder = client_builder(SwarmNodeType::Bootnode).with_swap(swap_with_chequebook());
        assert_eq!(effective_mode(&builder), BandwidthMode::Swap);
    }

    #[test]
    fn with_swap_without_a_chequebook_leaves_the_mode_untouched() {
        let builder = client_builder(SwarmNodeType::Client).with_swap(SwapConfig::default());
        assert!(
            builder.bandwidth_mode_override.is_none(),
            "without a chequebook the swap fold must not fire"
        );
        assert_eq!(effective_mode(&builder), BandwidthMode::Pseudosettle);
    }

    #[test]
    fn with_swap_is_idempotent() {
        let builder = client_builder(SwarmNodeType::Client)
            .with_swap(swap_with_chequebook())
            .with_swap(swap_with_chequebook());
        assert_eq!(effective_mode(&builder), BandwidthMode::Both);
    }

    #[test]
    fn from_config_does_not_fold_the_mode() {
        // The config round-trip records the chequebook but leaves SWAP selection to the mode.
        let identity = test_identity_arc();
        let config = ClientConfig::new(
            identity.spec().clone(),
            identity,
            test_network(),
            DefaultBandwidthConfig::for_node_type(SwarmNodeType::Client),
            LocalStoreConfig::default(),
            ChunkVerifyConfig::default(),
            ChainConfig::default(),
            SwapConfig {
                chequebook: Some(alloy_primitives::Address::repeat_byte(0x11)),
                ..SwapConfig::default()
            },
        );
        let builder = DefaultClientBuilder::from_config(config);
        assert!(
            builder.bandwidth_mode_override.is_none(),
            "from_config must not fold swap into the mode"
        );
        assert_eq!(effective_mode(&builder), BandwidthMode::Pseudosettle);
    }

    #[test]
    fn storer_with_swap_stays_both() {
        // Storer is seeded Both; the fold is a no-op.
        let identity = test_identity_arc();
        let builder = DefaultStorerBuilder::from_parts(
            identity.spec().clone(),
            identity,
            test_network(),
            DefaultBandwidthConfig::for_node_type(SwarmNodeType::Storer),
            LocalStoreConfig::default(),
            StorageConfig::new(false),
            ChunkVerifyConfig::default(),
        )
        .with_swap(SwapConfig::default());
        assert_eq!(effective_mode(&builder.client), BandwidthMode::Both);
    }
}
