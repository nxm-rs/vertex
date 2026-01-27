//! Configuration traits for Swarm protocol components.
//!
//! These traits define the configuration parameters that Swarm component builders need.
//! They cover protocol-level concerns: bandwidth accounting, storage, networking, and identity.
//!
//! # Design
//!
//! Following the reth pattern:
//! - Traits define *what* configuration is needed
//! - CLI args implement the traits directly (no intermediate structs)
//! - Builders receive `impl ConfigTrait` and extract what they need
//!
//! # Combined Config
//!
//! The [`SwarmConfig`] super-trait combines all protocol configs into one bound,
//! useful for builders that need access to everything.
//!
//! # Build Configuration
//!
//! [`SwarmBuildConfig`] defines how to construct components and services.
//! The capability level is encoded by:
//! - `Types` implementing [`LightTypes`], [`PublisherTypes`], or [`FullTypes`]
//! - `Components` being the corresponding components struct

use core::time::Duration;

use async_trait::async_trait;
use vertex_node_api::NodeContext;
use vertex_swarmspec::SwarmSpec;

use crate::{BootnodeTypes, FullTypes, LightTypes, PublisherTypes, SwarmServices};

/// Configuration for bandwidth incentives (pseudosettle / SWAP).
///
/// All values are in **Accounting Units (AU)**, not bytes or BZZ tokens.
///
/// # Defaults
///
/// - Base price: 10,000 AU per chunk
/// - Refresh rate: 4,500,000 AU/second (full node), 450,000 AU/second (light)
/// - Payment threshold: 13,500,000 AU
/// - Payment tolerance: 25% (disconnect = threshold × 1.25)
pub trait BandwidthIncentiveConfig {
    /// Whether pseudosettle (soft accounting) is enabled.
    fn pseudosettle_enabled(&self) -> bool;

    /// Whether SWAP (real payment channels) is enabled.
    fn swap_enabled(&self) -> bool;

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
    /// Actual price depends on proximity: (MAX_PO - proximity + 1) * base_price
    fn base_price(&self) -> u64;

    /// Refresh rate in accounting units per second (for pseudosettle).
    fn refresh_rate(&self) -> u64;

    /// Early payment threshold as a percentage (0-100).
    ///
    /// Settlement is triggered when debt exceeds (100 - early) % of threshold.
    fn early_payment_percent(&self) -> u64;

    /// Light node scaling factor.
    ///
    /// Light nodes have all thresholds and rates divided by this factor.
    fn light_factor(&self) -> u64;

    /// Calculate the disconnect threshold.
    fn disconnect_threshold(&self) -> u64 {
        self.payment_threshold() * (100 + self.payment_tolerance_percent()) / 100
    }

    /// Check if any bandwidth incentive is enabled.
    fn is_enabled(&self) -> bool {
        self.pseudosettle_enabled() || self.swap_enabled()
    }
}

/// Configuration for local chunk store.
///
/// Storage capacity is expressed in number of chunks, which is the natural
/// unit for Swarm storage. Use [`estimate_storage_bytes`] to calculate the
/// approximate disk space required for a given number of chunks.
pub trait StoreConfig {
    /// Maximum storage capacity in number of chunks.
    fn capacity_chunks(&self) -> u64;

    /// Cache capacity in number of chunks.
    fn cache_chunks(&self) -> u64;
}

/// Configuration for storage incentives (redistribution, postage).
pub trait StorageConfig {
    /// Whether this node participates in redistribution.
    fn redistribution_enabled(&self) -> bool;
}

/// Configuration for P2P networking.
pub trait NetworkConfig {
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
}

/// Configuration for Swarm node identity.
pub trait IdentityConfig {
    /// Whether to use ephemeral identity (random key, not persisted).
    ///
    /// When true, the node will generate a random identity on each start.
    /// When false (default), identity persistence depends on node type:
    /// - Light/Publisher nodes default to ephemeral unless keystore exists
    /// - Full/Staker nodes require persistent identity
    fn ephemeral(&self) -> bool;
}

/// Combined Swarm protocol configuration.
///
/// This super-trait combines all protocol-level configs into one bound.
pub trait SwarmConfig:
    BandwidthIncentiveConfig + StoreConfig + StorageConfig + NetworkConfig + IdentityConfig
{
}

impl<T> SwarmConfig for T where
    T: BandwidthIncentiveConfig + StoreConfig + StorageConfig + NetworkConfig + IdentityConfig
{
}

/// Cache capacity divisor relative to reserve capacity.
pub const CACHE_CAPACITY_DIVISOR: u64 = 64;

/// Implement [`StoreConfig`] for any [`SwarmSpec`].
impl<S: SwarmSpec> StoreConfig for S {
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

impl StorageConfig for DefaultStorageConfig {
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
/// - `Types` must implement the appropriate capability trait
///   ([`LightTypes`], [`PublisherTypes`], or [`FullTypes`])
/// - `Components` must be the corresponding components struct
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_api::{SwarmLaunchConfig, SwarmLightComponents, SwarmServices, LightTypes};
///
/// struct MyLightConfig { /* ... */ }
///
/// #[async_trait]
/// impl SwarmLaunchConfig for MyLightConfig {
///     type Types = MyLightTypes;  // Must implement LightTypes
///     type Components = SwarmLightComponents<MyLightTypes>;
///     type Error = MyError;
///
///     async fn build(self, ctx: &NodeContext)
///         -> Result<(Self::Components, SwarmServices<Self::Types>), Self::Error>
///     {
///         // Build identity, topology, accounting, services...
///     }
/// }
/// ```
#[async_trait]
pub trait SwarmLaunchConfig: Send + Sync + 'static {
    /// The Swarm types for this configuration.
    ///
    /// Must implement the capability trait for the desired node level:
    /// - [`LightTypes`] for light nodes
    /// - [`PublisherTypes`] for publisher nodes
    /// - [`FullTypes`] for full nodes
    type Types: BootnodeTypes;

    /// The components produced by building.
    ///
    /// Should match the capability level of `Types`:
    /// - [`SwarmLightComponents`](crate::SwarmLightComponents) for light nodes
    /// - [`SwarmPublisherComponents`](crate::SwarmPublisherComponents) for publishers
    /// - [`SwarmFullComponents`](crate::SwarmFullComponents) for full nodes
    type Components: Send + Sync + 'static;

    /// Error type for build failures.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Build components and services from this configuration.
    ///
    /// Services will be spawned by `SwarmProtocol::launch()`.
    async fn build(
        self,
        ctx: &NodeContext,
    ) -> Result<(Self::Components, SwarmServices<Self::Types>), Self::Error>;
}

/// Marker for configs that launch light nodes.
pub trait LightLaunchConfig: SwarmLaunchConfig
where
    Self::Types: LightTypes,
{
}
impl<T: SwarmLaunchConfig> LightLaunchConfig for T where T::Types: LightTypes {}

/// Marker for configs that launch publisher nodes.
pub trait PublisherLaunchConfig: SwarmLaunchConfig
where
    Self::Types: PublisherTypes,
{
}
impl<T: SwarmLaunchConfig> PublisherLaunchConfig for T where T::Types: PublisherTypes {}

/// Marker for configs that launch full nodes.
pub trait FullLaunchConfig: SwarmLaunchConfig
where
    Self::Types: FullTypes,
{
}
impl<T: SwarmLaunchConfig> FullLaunchConfig for T where T::Types: FullTypes {}
