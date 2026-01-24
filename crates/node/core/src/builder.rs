//! Node component builders using the type-state pattern.
//!
//! This module provides a type-safe builder for constructing Swarm nodes.
//! The builder tracks configured components at compile time, ensuring all
//! required components are provided before building.
//!
//! # Architecture
//!
//! The builder hierarchy follows the reth pattern:
//!
//! ```text
//! NodeBuilder                     (this module)
//! ├── SwarmComponentsBuilder      (from vertex-client-core)
//! │   ├── TopologyBuilder
//! │   ├── AccountingBuilder
//! │   └── PricerBuilder
//! └── Infrastructure Builders     (this module)
//!     ├── DatabaseBuilder
//!     └── RpcBuilder
//! ```
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_core::builder::NodeBuilder;
//!
//! // Build a light node with default components
//! let components = NodeBuilder::new()
//!     .with_spec(spec)
//!     .with_identity(identity)
//!     .with_swarm_components(DefaultComponentsBuilder::with_defaults())
//!     .build(&ctx);
//! ```

use std::sync::Arc;

use vertex_bandwidth_core::AccountingConfig;
use vertex_node_types::{DatabaseProvider, RpcServer};
use vertex_rpc_server::{GrpcServer, GrpcServerConfig};
use vertex_swarm_api::{
    AvailabilityAccounting, AvailabilityIncentiveConfig, ChunkSync, LightTypes, LocalStore,
    NetworkConfig, NoAvailabilityIncentives, StorageConfig, StoreConfig, Topology,
};
use vertex_swarmspec::SwarmSpec;

use crate::config::NodeType;

// Re-export SwarmComponentsBuilder from client-core for convenience
pub use vertex_client_core::{
    AccountingBuilder, BandwidthAccountingBuilder, BuiltSwarmComponents, DefaultComponentsBuilder,
    FixedPricerBuilder, KademliaTopologyBuilder, NoAccountingBuilder, PricerBuilder,
    SwarmBuilderContext, SwarmComponentsBuilder, TopologyBuilder,
};

// ============================================================================
// Marker types for unset builder fields
// ============================================================================

/// Marker type indicating a builder field has not been set.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unset;

/// Marker type for no storage incentives (light nodes).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStorageIncentive;

/// Marker type for no local store (light nodes).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStore;

/// Marker type for no sync (light nodes).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSync;

/// Marker type for no identity configured yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoIdentity;

// ============================================================================
// Infrastructure Builder Traits
// ============================================================================

/// Builder context for infrastructure components.
///
/// Provides runtime state for building database, RPC, etc.
pub struct NodeBuilderContext<'cfg, Types: LightTypes, Cfg: NetworkConfig> {
    /// Swarm builder context (identity, config, executor).
    pub swarm_ctx: SwarmBuilderContext<'cfg, Types, Cfg>,

    /// Data directory root.
    pub data_dir: Option<&'cfg std::path::Path>,
}

impl<'cfg, Types: LightTypes, Cfg: NetworkConfig> NodeBuilderContext<'cfg, Types, Cfg> {
    /// Create from a swarm builder context.
    pub fn new(swarm_ctx: SwarmBuilderContext<'cfg, Types, Cfg>) -> Self {
        Self {
            swarm_ctx,
            data_dir: None,
        }
    }

    /// Set the data directory.
    pub fn with_data_dir(mut self, data_dir: &'cfg std::path::Path) -> Self {
        self.data_dir = Some(data_dir);
        self
    }
}

/// Builds a database provider.
pub trait DatabaseBuilder<Types: LightTypes, Cfg: NetworkConfig>: Send + Sync + 'static {
    /// The database type produced.
    type Database: DatabaseProvider;

    /// Build the database given the context.
    fn build_database(self, ctx: &NodeBuilderContext<'_, Types, Cfg>) -> Self::Database;
}

/// Builds an RPC server.
pub trait RpcBuilder<Types: LightTypes, Cfg: NetworkConfig>: Send + Sync + 'static {
    /// The RPC server type produced.
    type Rpc: RpcServer;

    /// Build the RPC server given the context.
    fn build_rpc(self, ctx: &NodeBuilderContext<'_, Types, Cfg>) -> Self::Rpc;
}

// ============================================================================
// Default Infrastructure Builders
// ============================================================================

/// No-op database builder (for nodes without persistence).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDatabaseBuilder;

impl<Types: LightTypes, Cfg: NetworkConfig> DatabaseBuilder<Types, Cfg> for NoDatabaseBuilder {
    type Database = ();

    fn build_database(self, _ctx: &NodeBuilderContext<'_, Types, Cfg>) -> Self::Database {}
}

/// No-op RPC builder (for nodes without RPC).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRpcBuilder;

impl<Types: LightTypes, Cfg: NetworkConfig> RpcBuilder<Types, Cfg> for NoRpcBuilder {
    type Rpc = ();

    fn build_rpc(self, _ctx: &NodeBuilderContext<'_, Types, Cfg>) -> Self::Rpc {}
}

/// gRPC server builder.
#[derive(Debug, Clone)]
pub struct GrpcServerBuilder {
    config: GrpcServerConfig,
}

impl GrpcServerBuilder {
    /// Create with config.
    pub fn with_config(config: GrpcServerConfig) -> Self {
        Self { config }
    }

    /// Create with address.
    pub fn with_addr(addr: std::net::SocketAddr) -> Self {
        Self {
            config: GrpcServerConfig {
                addr,
                topology_provider: None,
            },
        }
    }
}

impl<Types: LightTypes, Cfg: NetworkConfig> RpcBuilder<Types, Cfg> for GrpcServerBuilder {
    type Rpc = Arc<GrpcServer>;

    fn build_rpc(self, _ctx: &NodeBuilderContext<'_, Types, Cfg>) -> Self::Rpc {
        GrpcServer::with_config(self.config)
    }
}

// ============================================================================
// Node Builder
// ============================================================================

/// Type-safe builder for Swarm nodes.
///
/// The builder uses generics to track which components have been configured.
/// This provides compile-time guarantees that all required components are
/// set before building.
///
/// # Type Parameters
///
/// - `Spec` - Network specification type
/// - `Ident` - Node identity type (signing key, overlay address)
/// - `Topo` - Topology type for peer discovery
/// - `Avail` - Availability accounting type
/// - `Storage` - Storage incentives type (postage stamps for publishers)
/// - `Store` - Local storage type (for full nodes)
/// - `Sync` - Chunk sync type (for full nodes)
#[derive(Debug)]
pub struct NodeBuilder<
    Spec = Unset,
    Ident = NoIdentity,
    Topo = Unset,
    Avail = NoAvailabilityIncentives,
    Storage = NoStorageIncentive,
    Store = NoStore,
    Syncer = NoSync,
> {
    spec: Spec,
    identity: Ident,
    topology: Topo,
    accounting: Avail,
    storage: Storage,
    store: Store,
    sync: Syncer,
}

impl Default for NodeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeBuilder {
    /// Create a new node builder with default (unset) components.
    pub fn new() -> Self {
        Self {
            spec: Unset,
            identity: NoIdentity,
            topology: Unset,
            accounting: NoAvailabilityIncentives,
            storage: NoStorageIncentive,
            store: NoStore,
            sync: NoSync,
        }
    }
}

impl<Spec, Ident, Topo, Avail, Storage, Store, Syncer>
    NodeBuilder<Spec, Ident, Topo, Avail, Storage, Store, Syncer>
{
    /// Set the network specification.
    ///
    /// The spec defines which network to connect to (mainnet, testnet, dev),
    /// hardfork schedule, bootnodes, and contract addresses.
    pub fn with_spec<NewSpec: SwarmSpec + Clone>(
        self,
        spec: NewSpec,
    ) -> NodeBuilder<NewSpec, Ident, Topo, Avail, Storage, Store, Syncer> {
        NodeBuilder {
            spec,
            identity: self.identity,
            topology: self.topology,
            accounting: self.accounting,
            storage: self.storage,
            store: self.store,
            sync: self.sync,
        }
    }

    /// Set the node identity.
    ///
    /// The identity contains the signing key, nonce, and derived overlay address.
    /// This is fundamental to the node's participation in the network.
    pub fn with_identity<NewIdent: Clone + Send + Sync + 'static>(
        self,
        identity: NewIdent,
    ) -> NodeBuilder<Spec, NewIdent, Topo, Avail, Storage, Store, Syncer> {
        NodeBuilder {
            spec: self.spec,
            identity,
            topology: self.topology,
            accounting: self.accounting,
            storage: self.storage,
            store: self.store,
            sync: self.sync,
        }
    }

    /// Set the topology implementation.
    ///
    /// The topology manages peer discovery and routing in the Kademlia DHT.
    pub fn with_topology<NewTopo: Topology + Clone>(
        self,
        topology: NewTopo,
    ) -> NodeBuilder<Spec, Ident, NewTopo, Avail, Storage, Store, Syncer> {
        NodeBuilder {
            spec: self.spec,
            identity: self.identity,
            topology,
            accounting: self.accounting,
            storage: self.storage,
            store: self.store,
            sync: self.sync,
        }
    }

    /// Set the availability accounting implementation.
    ///
    /// This determines how bandwidth usage is tracked and settled:
    /// - `NoAvailabilityIncentives` - No accounting (bootnodes, testing)
    /// - `PseudosettleAccounting` - Time-based refresh allowance
    /// - `SwapAccounting` - Chequebook-based payment
    /// - `PseudosettleSwap` - Both mechanisms combined
    pub fn with_accounting<NewAvail: AvailabilityAccounting>(
        self,
        accounting: NewAvail,
    ) -> NodeBuilder<Spec, Ident, Topo, NewAvail, Storage, Store, Syncer> {
        NodeBuilder {
            spec: self.spec,
            identity: self.identity,
            topology: self.topology,
            accounting,
            storage: self.storage,
            store: self.store,
            sync: self.sync,
        }
    }

    /// Set the storage incentives mechanism.
    ///
    /// For publisher and full nodes, this is typically postage stamps.
    /// Light nodes don't need storage incentives.
    pub fn with_storage<NewStorage: Send + Sync + 'static>(
        self,
        storage: NewStorage,
    ) -> NodeBuilder<Spec, Ident, Topo, Avail, NewStorage, Store, Syncer> {
        NodeBuilder {
            spec: self.spec,
            identity: self.identity,
            topology: self.topology,
            accounting: self.accounting,
            storage,
            store: self.store,
            sync: self.sync,
        }
    }

    /// Set the local storage implementation.
    ///
    /// Full nodes need local storage to persist chunks they're responsible for.
    pub fn with_store<NewStore: LocalStore + Clone>(
        self,
        store: NewStore,
    ) -> NodeBuilder<Spec, Ident, Topo, Avail, Storage, NewStore, Syncer> {
        NodeBuilder {
            spec: self.spec,
            identity: self.identity,
            topology: self.topology,
            accounting: self.accounting,
            storage: self.storage,
            store,
            sync: self.sync,
        }
    }

    /// Set the chunk synchronization implementation.
    ///
    /// Full nodes need sync to exchange chunks with neighbors.
    pub fn with_sync<NewSync: ChunkSync + Clone>(
        self,
        sync: NewSync,
    ) -> NodeBuilder<Spec, Ident, Topo, Avail, Storage, Store, NewSync> {
        NodeBuilder {
            spec: self.spec,
            identity: self.identity,
            topology: self.topology,
            accounting: self.accounting,
            storage: self.storage,
            store: self.store,
            sync,
        }
    }
}

// ============================================================================
// Accessor methods
// ============================================================================

impl<Spec, Ident, Topo, Avail, Storage, Store, Syncer>
    NodeBuilder<Spec, Ident, Topo, Avail, Storage, Store, Syncer>
{
    /// Get a reference to the configured spec.
    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Get a reference to the configured identity.
    pub fn identity(&self) -> &Ident {
        &self.identity
    }

    /// Get a reference to the configured topology.
    pub fn topology(&self) -> &Topo {
        &self.topology
    }

    /// Get a reference to the configured accounting.
    pub fn accounting(&self) -> &Avail {
        &self.accounting
    }

    /// Get a reference to the configured storage incentives.
    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    /// Get a reference to the configured store.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Get a reference to the configured sync.
    pub fn sync(&self) -> &Syncer {
        &self.sync
    }

    /// Consume the builder and return all components as a tuple.
    pub fn into_parts(self) -> (Spec, Ident, Topo, Avail, Storage, Store, Syncer) {
        (
            self.spec,
            self.identity,
            self.topology,
            self.accounting,
            self.storage,
            self.store,
            self.sync,
        )
    }
}

// ============================================================================
// Accounting Config Helper
// ============================================================================

/// Build an `AccountingConfig` from a trait implementation.
///
/// This is a helper for creating accounting instances from config traits.
pub fn accounting_config_from(config: &impl AvailabilityIncentiveConfig) -> AccountingConfig {
    AccountingConfig {
        payment_threshold: config.payment_threshold(),
        payment_tolerance_percent: config.payment_tolerance_percent(),
        disconnect_threshold: config.disconnect_threshold(),
        light_factor: config.light_factor(),
        base_price: config.base_price(),
        refresh_rate: config.refresh_rate(),
        early_payment_percent: config.early_payment_percent(),
    }
}

// ============================================================================
// Storage Parameters
// ============================================================================

/// Storage configuration extracted from traits.
#[derive(Debug, Clone)]
pub struct StorageParams {
    /// Maximum storage capacity in number of chunks.
    pub capacity_chunks: u64,
    /// Cache capacity in number of chunks.
    pub cache_chunks: u64,
    /// Whether redistribution is enabled.
    pub redistribution_enabled: bool,
}

impl StorageParams {
    /// Create storage params from config traits.
    pub fn from_config(storage: &impl StoreConfig, incentives: &impl StorageConfig) -> Self {
        Self {
            capacity_chunks: storage.capacity_chunks(),
            cache_chunks: storage.cache_chunks(),
            redistribution_enabled: incentives.redistribution_enabled(),
        }
    }
}

// ============================================================================
// Validation
// ============================================================================

/// Validate that the configuration is consistent with the node type.
pub fn validate_config(
    node_type: NodeType,
    availability: &impl AvailabilityIncentiveConfig,
    storage: &impl StoreConfig,
    storage_incentives: &impl StorageConfig,
) -> eyre::Result<()> {
    // Staker requires redistribution
    if matches!(node_type, NodeType::Staker) && !storage_incentives.redistribution_enabled() {
        return Err(eyre::eyre!(
            "Staker node type requires redistribution to be enabled"
        ));
    }

    // Full nodes should have storage capacity
    if node_type.requires_pullsync() && storage.capacity_chunks() == 0 {
        return Err(eyre::eyre!(
            "Full and Staker nodes require storage capacity > 0"
        ));
    }

    // SWAP requires some payment threshold
    if availability.swap_enabled() && availability.payment_threshold() == 0 {
        return Err(eyre::eyre!("SWAP enabled but payment threshold is 0"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::PseudosettleSwap;
    use vertex_bandwidth_core::{DefaultAvailabilityConfig, NoAvailabilityConfig};
    use vertex_bandwidth_pseudosettle::PseudosettleAccounting;
    use vertex_bandwidth_swap::SwapAccounting;
    use vertex_swarm_api::{
        AvailabilityAccounting, DefaultStorageConfig, StoreConfig, CACHE_CAPACITY_DIVISOR,
    };
    use vertex_swarmspec::init_testnet;

    #[test]
    fn test_builder_default() {
        let builder = NodeBuilder::new();
        // Default accounting is NoAvailabilityIncentives
        let _peers = builder.accounting().peers();
    }

    #[test]
    fn test_builder_with_accounting_pseudosettle() {
        let accounting = PseudosettleAccounting::with_default_refresh(Default::default());
        let builder = NodeBuilder::new().with_accounting(accounting);
        let _peers = builder.accounting().peers();
    }

    #[test]
    fn test_builder_with_accounting_swap() {
        let accounting = SwapAccounting::new(Default::default());
        let builder = NodeBuilder::new().with_accounting(accounting);
        let _peers = builder.accounting().peers();
    }

    #[test]
    fn test_builder_with_accounting_combined() {
        let accounting = PseudosettleSwap::with_default_refresh(Default::default());
        let builder = NodeBuilder::new().with_accounting(accounting);
        let _peers = builder.accounting().peers();
    }

    #[test]
    fn test_builder_with_identity() {
        use vertex_node_identity::SwarmIdentity;
        use vertex_swarmspec::init_testnet;

        let spec = init_testnet();
        let identity = SwarmIdentity::random(spec, false);
        let builder = NodeBuilder::new().with_identity(identity);

        // Can access identity through builder
        let _ = builder.identity();
    }

    #[test]
    fn test_builder_chaining() {
        // Test that multiple with_* calls can be chained
        let accounting = PseudosettleSwap::with_default_refresh(Default::default());
        let builder = NodeBuilder::new()
            .with_accounting(accounting)
            .with_storage(()); // Unit type for no payment

        let _ = builder.accounting();
        let _ = builder.storage();
    }

    #[test]
    fn test_builder_into_parts() {
        let accounting = PseudosettleSwap::with_default_refresh(Default::default());
        let builder = NodeBuilder::new().with_accounting(accounting);
        let (_spec, _identity, _topo, accounting, _storage, _store, _sync) = builder.into_parts();
        let _peers = accounting.peers();
    }

    #[test]
    fn test_storage_params_from_config() {
        let spec = init_testnet();
        let incentives = DefaultStorageConfig;
        let params = StorageParams::from_config(spec.as_ref(), &incentives);

        // Spec provides reserve_capacity, cache is reserve_capacity / 64
        assert_eq!(params.capacity_chunks, spec.reserve_capacity());
        assert_eq!(
            params.cache_chunks,
            spec.reserve_capacity() / CACHE_CAPACITY_DIVISOR
        );
        assert!(!params.redistribution_enabled);
    }

    #[test]
    fn test_validate_config_staker_needs_redistribution() {
        let availability = DefaultAvailabilityConfig;
        let spec = init_testnet();
        let storage_incentives = DefaultStorageConfig;

        let result = validate_config(
            NodeType::Staker,
            &availability,
            spec.as_ref(),
            &storage_incentives,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_config_light_node_ok() {
        let availability = NoAvailabilityConfig;
        let spec = init_testnet();
        let storage_incentives = DefaultStorageConfig;

        let result = validate_config(
            NodeType::Light,
            &availability,
            spec.as_ref(),
            &storage_incentives,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_config_full_node_needs_capacity() {
        struct ZeroCapacityStorage;
        impl StoreConfig for ZeroCapacityStorage {
            fn capacity_chunks(&self) -> u64 {
                0
            }
            fn cache_chunks(&self) -> u64 {
                0
            }
        }

        let availability = DefaultAvailabilityConfig;
        let storage_incentives = DefaultStorageConfig;
        let result = validate_config(
            NodeType::Full,
            &availability,
            &ZeroCapacityStorage,
            &storage_incentives,
        );
        assert!(result.is_err());
    }
}
