//! Configuration traits for Swarm protocol components.

use core::time::Duration;

use libp2p::Multiaddr;
use vertex_node_api::InfrastructureContext;

use crate::components::{SwarmAccountingConfig, SwarmLocalStoreConfig, SwarmPricingConfig};
use crate::{SwarmClientTypes, SwarmNetworkTypes, SwarmStorerTypes};

// Re-export from vertex-tasks (canonical location)
pub use vertex_tasks::{NodeTask, NodeTaskFn};

/// Configuration for storage incentives (redistribution, postage).
pub trait SwarmStorageConfig {
    /// Whether this node participates in redistribution.
    fn redistribution_enabled(&self) -> bool;
}

/// Configuration for Swarm routing.
///
/// The associated `Routing` type allows different routing implementations
/// (Kademlia, etc.) to define their own configuration structs.
pub trait SwarmRoutingConfig {
    /// The routing-specific configuration type.
    type Routing: Default;

    /// Get the routing configuration.
    fn routing(&self) -> &Self::Routing;
}

/// Default ban threshold for peer scoring (-100.0).
pub const DEFAULT_PEER_BAN_THRESHOLD: f64 = -100.0;

/// Default warn threshold for peer scoring (-50.0).
pub const DEFAULT_PEER_WARN_THRESHOLD: f64 = -50.0;

/// Default maximum peers per proximity bin in the index.
pub const DEFAULT_PEER_MAX_PER_BIN: usize = 128;

/// Configuration for peer management (scoring, limits).
pub trait SwarmPeerConfig {
    /// The peer management configuration type.
    type Peers: Default + PeerConfigValues;

    /// Get the peer configuration.
    fn peers(&self) -> &Self::Peers;
}

/// Values required from a peer configuration.
pub trait PeerConfigValues {
    /// Score threshold below which peers are banned.
    fn ban_threshold(&self) -> f64;

    /// Score threshold below which a warning is emitted.
    fn warn_threshold(&self) -> f64 {
        DEFAULT_PEER_WARN_THRESHOLD
    }

    /// Maximum peers per proximity bin in the index.
    fn max_per_bin(&self) -> usize {
        DEFAULT_PEER_MAX_PER_BIN
    }

    /// Path for peer store persistence. None uses ephemeral in-memory storage.
    fn store_path(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// Default peer management configuration.
#[derive(Debug, Clone)]
pub struct DefaultPeerConfig {
    /// Score threshold for banning peers.
    pub ban_threshold: f64,
    /// Score threshold for warning about peers.
    pub warn_threshold: f64,
    /// Maximum peers per proximity bin.
    pub max_per_bin: usize,
    /// Path for peer store persistence.
    pub store_path: Option<std::path::PathBuf>,
}

impl Default for DefaultPeerConfig {
    fn default() -> Self {
        Self {
            ban_threshold: DEFAULT_PEER_BAN_THRESHOLD,
            warn_threshold: DEFAULT_PEER_WARN_THRESHOLD,
            max_per_bin: DEFAULT_PEER_MAX_PER_BIN,
            store_path: None,
        }
    }
}

impl PeerConfigValues for DefaultPeerConfig {
    fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }

    fn warn_threshold(&self) -> f64 {
        self.warn_threshold
    }

    fn max_per_bin(&self) -> usize {
        self.max_per_bin
    }

    fn store_path(&self) -> Option<std::path::PathBuf> {
        self.store_path.clone()
    }
}

/// Configuration for P2P networking.
///
/// Address methods return parsed `Multiaddr` to ensure validation happens early.
/// Implementors should validate and parse addresses at construction time
/// (e.g., in `TryFrom` or a constructor that returns `Result`).
pub trait SwarmNetworkConfig {
    /// Listen addresses (parsed).
    fn listen_addrs(&self) -> &[Multiaddr];

    /// Bootnode addresses (parsed).
    fn bootnodes(&self) -> &[Multiaddr];

    /// Trusted peer addresses (parsed).
    fn trusted_peers(&self) -> &[Multiaddr] {
        &[]
    }

    /// Whether peer discovery is enabled.
    fn discovery_enabled(&self) -> bool;

    /// Maximum number of peer connections.
    fn max_peers(&self) -> usize;

    /// Connection idle timeout.
    fn idle_timeout(&self) -> Duration;

    /// External/NAT addresses to advertise (parsed).
    fn nat_addrs(&self) -> &[Multiaddr] {
        &[]
    }

    /// Whether auto-NAT discovery from observed addresses is enabled (default: true).
    fn nat_auto_enabled(&self) -> bool {
        true
    }
}

/// Configuration for Swarm node identity.
pub trait SwarmIdentityConfig {
    /// Whether to use ephemeral identity (random key, not persisted).
    fn ephemeral(&self) -> bool;
}

/// Base configuration for all Swarm nodes (bootnode level).
///
/// Provides P2P networking and routing configuration needed by any node that
/// participates in the Swarm overlay network. Identity configuration
/// (`SwarmIdentityConfig`) is separate since identity is created before node
/// building and passed in directly.
///
/// This is the foundation of the config hierarchy:
/// - `SwarmBootnodeConfig` - networking + peer management + routing (this trait)
/// - `SwarmClientConfig` - adds accounting + pricing
/// - `SwarmStorerConfig` - adds local storage + redistribution
pub trait SwarmBootnodeConfig: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig {}

impl<T> SwarmBootnodeConfig for T where T: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig {}

/// Configuration for client nodes.
///
/// Extends bootnode config with bandwidth accounting and chunk pricing,
/// enabling the node to retrieve and upload chunks with proper payment.
pub trait SwarmClientConfig:
    SwarmBootnodeConfig + SwarmAccountingConfig + SwarmPricingConfig
{
}

impl<T> SwarmClientConfig for T where
    T: SwarmBootnodeConfig + SwarmAccountingConfig + SwarmPricingConfig
{
}

/// Configuration for storer (full) nodes.
///
/// Extends client config with local chunk storage and redistribution,
/// enabling the node to store chunks and participate in storage incentives.
pub trait SwarmStorerConfig:
    SwarmClientConfig + SwarmLocalStoreConfig + SwarmStorageConfig
{
}

impl<T> SwarmStorerConfig for T where
    T: SwarmClientConfig + SwarmLocalStoreConfig + SwarmStorageConfig
{
}

/// Default storage incentive configuration (redistribution disabled).
#[derive(Debug, Clone, Copy)]
pub struct DefaultStorageConfig;

impl SwarmStorageConfig for DefaultStorageConfig {
    fn redistribution_enabled(&self) -> bool {
        false
    }
}

/// Estimated metadata overhead as a fraction of chunk data.
pub const METADATA_OVERHEAD_FACTOR: f64 = 0.20;

/// Estimate the total disk space required for storing a given number of chunks.
///
/// # Example
/// ```
/// use vertex_swarm_api::estimate_storage_bytes;
///
/// let bytes = estimate_storage_bytes(1_000_000, 4096);
/// assert_eq!(bytes, 4_915_200_000); // ~4.58 GB
/// ```
pub fn estimate_storage_bytes(num_chunks: u64, chunk_size: usize) -> u64 {
    let chunk_data = num_chunks * chunk_size as u64;
    let overhead = (chunk_data as f64 * METADATA_OVERHEAD_FACTOR) as u64;
    chunk_data + overhead
}

/// Estimate the number of chunks that can fit in a given disk space.
///
/// # Example
/// ```
/// use vertex_swarm_api::estimate_chunks_for_bytes;
///
/// let chunks = estimate_chunks_for_bytes(10 * 1024 * 1024 * 1024, 4096);
/// assert_eq!(chunks, 2_184_533);
/// ```
pub fn estimate_chunks_for_bytes(available_bytes: u64, chunk_size: usize) -> u64 {
    let effective_chunk_size = chunk_size as f64 * (1.0 + METADATA_OVERHEAD_FACTOR);
    (available_bytes as f64 / effective_chunk_size) as u64
}

/// Configuration that knows how to launch a Swarm node.
///
/// Build produces a task function (accepting graceful shutdown) and providers for RPC.
/// The provider type varies by node capability (client vs storer).
#[async_trait::async_trait]
pub trait SwarmLaunchConfig: Send + Sync + 'static {
    /// The Swarm types for this configuration.
    type Types: SwarmNetworkTypes;

    /// Providers for RPC services (node-type specific).
    type Providers: Send + Sync + 'static;

    /// Error type for build failures.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Build the node's main event loop and RPC providers.
    ///
    /// Returns a task function that accepts a `GracefulShutdown` signal.
    /// When the signal fires, the task should clean up and exit gracefully.
    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error>;
}

/// Launch config for client (light) nodes.
///
/// Client nodes can retrieve and upload chunks but don't store them locally.
/// This is the default node type for most users.
pub trait SwarmClientLaunchConfig: SwarmLaunchConfig
where
    Self::Types: SwarmClientTypes,
{
    /// Called after successful build to perform client-specific initialization.
    fn on_client_ready(&self) {
        // Default no-op, override for custom initialization
    }
}

impl<T: SwarmLaunchConfig> SwarmClientLaunchConfig for T where T::Types: SwarmClientTypes {}

/// Launch config for storer (full) nodes.
///
/// Storer nodes store chunks locally and participate in the storage incentive
/// system (redistribution). They earn rewards for providing storage.
pub trait SwarmStorerLaunchConfig: SwarmLaunchConfig
where
    Self::Types: SwarmStorerTypes,
{
    /// Called after successful build to perform storer-specific initialization.
    fn on_storer_ready(&self) {
        // Default no-op, override for custom initialization
    }

    /// Whether this storer participates in redistribution.
    ///
    /// Override to enable redistribution game participation.
    fn redistribution_enabled(&self) -> bool {
        false
    }
}

impl<T: SwarmLaunchConfig> SwarmStorerLaunchConfig for T where T::Types: SwarmStorerTypes {}
