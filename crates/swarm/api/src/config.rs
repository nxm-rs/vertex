//! Configuration traits for Swarm protocol components.

use core::time::Duration;

use vertex_node_api::NodeContext;
use vertex_swarmspec::SwarmSpec;

use crate::{SwarmBootnodeTypes, SwarmClientTypes, SwarmStorerTypes, Services};

pub use vertex_swarm_primitives::BandwidthMode;

/// Configuration for bandwidth accounting (pseudosettle / SWAP).
///
/// All values are in Accounting Units (AU), not bytes or BZZ tokens.
pub trait SwarmAccountingConfig: Send + Sync {
    /// The bandwidth accounting mode.
    fn mode(&self) -> BandwidthMode;

    /// Whether pseudosettle (soft accounting) is enabled.
    fn pseudosettle_enabled(&self) -> bool {
        self.mode().pseudosettle_enabled()
    }

    /// Whether SWAP (real payment channels) is enabled.
    fn swap_enabled(&self) -> bool {
        self.mode().swap_enabled()
    }

    /// Payment threshold in accounting units.
    ///
    /// When a peer's debt reaches this threshold, settlement is requested.
    fn payment_threshold(&self) -> u64;

    /// Payment tolerance as a percentage (0-100).
    ///
    /// Disconnect threshold = payment_threshold * (100 + tolerance) / 100
    fn payment_tolerance_percent(&self) -> u64;

    /// Base price per chunk in accounting units.
    ///
    /// Actual price depends on proximity: (max_po - proximity + 1) * base_price
    fn base_price(&self) -> u64;

    /// Refresh rate in accounting units per second (for pseudosettle).
    fn refresh_rate(&self) -> u64;

    /// Early payment threshold as a percentage (0-100).
    ///
    /// Settlement is triggered when debt exceeds (100 - early) % of threshold.
    fn early_payment_percent(&self) -> u64;

    /// Client node scaling factor.
    ///
    /// Client nodes have all thresholds and rates divided by this factor.
    fn light_factor(&self) -> u64;

    /// Calculate the disconnect threshold.
    fn disconnect_threshold(&self) -> u64 {
        self.payment_threshold() * (100 + self.payment_tolerance_percent()) / 100
    }

    /// Check if any bandwidth incentive is enabled.
    fn is_enabled(&self) -> bool {
        self.mode().is_enabled()
    }
}

/// Default accounting configuration with pseudosettle enabled.
#[derive(Clone, Copy, Default)]
pub struct DefaultAccountingConfig;

impl SwarmAccountingConfig for DefaultAccountingConfig {
    fn mode(&self) -> BandwidthMode {
        BandwidthMode::Pseudosettle
    }

    fn payment_threshold(&self) -> u64 {
        13_500_000
    }

    fn payment_tolerance_percent(&self) -> u64 {
        25
    }

    fn base_price(&self) -> u64 {
        10_000
    }

    fn refresh_rate(&self) -> u64 {
        4_500_000
    }

    fn early_payment_percent(&self) -> u64 {
        50
    }

    fn light_factor(&self) -> u64 {
        10
    }
}

/// Configuration for local chunk store (capacity in chunks).
pub trait SwarmStoreConfig {
    /// Maximum storage capacity in number of chunks.
    fn capacity_chunks(&self) -> u64;

    /// Cache capacity in number of chunks.
    fn cache_chunks(&self) -> u64;
}

/// Configuration for storage incentives (redistribution, postage).
pub trait SwarmStorageConfig {
    /// Whether this node participates in redistribution.
    fn redistribution_enabled(&self) -> bool;
}

/// Configuration for P2P networking.
pub trait SwarmNetworkConfig {
    /// Get listen addresses as multiaddr strings.
    fn listen_addrs(&self) -> Vec<String>;

    /// Get bootnode addresses as multiaddr strings.
    fn bootnodes(&self) -> Vec<String>;

    /// Whether peer discovery is enabled.
    fn discovery_enabled(&self) -> bool;

    /// Maximum number of peer connections.
    fn max_peers(&self) -> usize;

    /// Connection idle timeout.
    fn idle_timeout(&self) -> Duration;

    /// Get external/NAT addresses to advertise.
    fn nat_addrs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether auto-NAT discovery from observed addresses is enabled.
    fn nat_auto_enabled(&self) -> bool {
        false
    }
}

/// Configuration for Swarm node identity.
pub trait SwarmIdentityConfig {
    /// Whether to use ephemeral identity (random key, not persisted).
    fn ephemeral(&self) -> bool;
}

/// Combined Swarm protocol configuration.
pub trait SwarmConfig:
    SwarmAccountingConfig + SwarmStoreConfig + SwarmStorageConfig + SwarmNetworkConfig + SwarmIdentityConfig
{
}

impl<T> SwarmConfig for T where
    T: SwarmAccountingConfig + SwarmStoreConfig + SwarmStorageConfig + SwarmNetworkConfig + SwarmIdentityConfig
{
}

/// Cache capacity divisor relative to reserve capacity.
pub const CACHE_CAPACITY_DIVISOR: u64 = 64;

/// Implement [`SwarmStoreConfig`] for any [`SwarmSpec`].
impl<S: SwarmSpec> SwarmStoreConfig for S {
    fn capacity_chunks(&self) -> u64 {
        self.reserve_capacity()
    }

    fn cache_chunks(&self) -> u64 {
        self.reserve_capacity() / CACHE_CAPACITY_DIVISOR
    }
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
/// The capability level is determined by the associated types:
/// - `Types` must implement [`SwarmClientTypes`] or [`SwarmStorerTypes`]
/// - `Components` must be the corresponding components struct
#[async_trait::async_trait]
pub trait SwarmLaunchConfig: Send + Sync + 'static {
    /// The Swarm types for this configuration.
    type Types: SwarmBootnodeTypes;

    /// The components produced by building.
    type Components: Send + Sync + 'static;

    /// Error type for build failures.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Build components and services from this configuration.
    async fn build(
        self,
        ctx: &NodeContext,
    ) -> Result<(Self::Components, Services<Self::Types>), Self::Error>;
}

/// Marker for configs that launch client nodes.
pub trait SwarmClientLaunchConfig: SwarmLaunchConfig
where
    Self::Types: SwarmClientTypes,
{
}
impl<T: SwarmLaunchConfig> SwarmClientLaunchConfig for T where T::Types: SwarmClientTypes {}

/// Marker for configs that launch storer nodes.
pub trait SwarmStorerLaunchConfig: SwarmLaunchConfig
where
    Self::Types: SwarmStorerTypes,
{
}
impl<T: SwarmLaunchConfig> SwarmStorerLaunchConfig for T where T::Types: SwarmStorerTypes {}
