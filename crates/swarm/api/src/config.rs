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
//! # Example
//!
//! ```ignore
//! // CLI args implement config traits
//! impl AvailabilityIncentiveConfig for AvailabilityArgs {
//!     fn pseudosettle_enabled(&self) -> bool { ... }
//!     fn payment_threshold(&self) -> u64 { self.payment_threshold }
//! }
//!
//! // Builder uses the trait
//! fn build_accounting(config: &impl AvailabilityIncentiveConfig) -> impl AvailabilityAccounting {
//!     if config.pseudosettle_enabled() {
//!         PseudosettleAccounting::new(config.payment_threshold())
//!     } else {
//!         NoAvailabilityIncentives
//!     }
//! }
//! ```

use core::time::Duration;

use vertex_swarmspec::SwarmSpec;

/// Configuration for availability incentives (pseudosettle / SWAP).
///
/// All values are in **Accounting Units (AU)**, not bytes or BZZ tokens.
/// This matches Bee's accounting system.
///
/// # Bee Compatibility
///
/// - Base price: 10,000 AU per chunk
/// - Refresh rate: 4,500,000 AU/second (full node), 450,000 AU/second (light)
/// - Payment threshold: 13,500,000 AU
/// - Payment tolerance: 25% (disconnect = threshold × 1.25)
pub trait AvailabilityIncentiveConfig {
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

    /// Check if any availability incentive is enabled.
    fn is_enabled(&self) -> bool {
        self.pseudosettle_enabled() || self.swap_enabled()
    }
}

/// Configuration for local chunk store.
///
/// Storage capacity is expressed in number of chunks, which is the natural
/// unit for Swarm storage. Use [`estimate_storage_bytes`] to calculate the
/// approximate disk space required for a given number of chunks.
///
/// # Storage Estimation
///
/// The actual disk space required depends on:
/// - Chunk data: `capacity_chunks * chunk_size` (e.g., 4096 bytes per chunk)
/// - Metadata overhead: indexes, leveldb overhead, etc. (~20% typical)
///
/// Example: 1,000,000 chunks at 4KB each = ~4GB chunk data + ~800MB metadata ≈ 4.8GB
///
/// # Spec-Aware Defaults
///
/// The [`SpecBasedStoreConfig`] implementation derives defaults from the
/// [`SwarmSpec`], using `reserve_capacity()` for capacity and a fraction
/// of that for cache.
pub trait StoreConfig {
    /// Maximum storage capacity in number of chunks.
    ///
    /// This determines how many chunks the node can store (Full/Staker nodes).
    /// The actual disk space required will be approximately
    /// `chunks * chunk_size * 1.2` to account for metadata overhead.
    ///
    /// Default: `SwarmSpec::reserve_capacity()` (2^22 chunks on mainnet)
    fn capacity_chunks(&self) -> u64;

    /// Cache capacity in number of chunks.
    ///
    /// This is an in-memory cache for frequently accessed chunks, used by
    /// non-storage nodes (Light/Publisher) for retrieval and pushsync.
    /// Ephemeral nodes can use memory-only caching with no disk persistence.
    ///
    /// Default: `capacity_chunks / 64` (2^16 chunks when capacity is 2^22)
    fn cache_chunks(&self) -> u64;
}

/// Configuration for storage incentives (redistribution, postage).
pub trait StorageConfig {
    /// Whether this node participates in redistribution.
    ///
    /// When enabled, the node participates in the redistribution game to earn
    /// rewards for storing chunks in its neighborhood.
    fn redistribution_enabled(&self) -> bool;
}

/// Configuration for P2P networking.
///
/// Implementations provide the parameters needed to configure the libp2p
/// swarm and peer connections.
pub trait NetworkConfig {
    /// Get listen addresses as multiaddr strings (e.g., "/ip4/0.0.0.0/tcp/1634").
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
///
/// The identity determines the node's overlay address (position in Kademlia DHT)
/// and its storage responsibilities. Full nodes need persistent identity to
/// maintain consistent storage responsibility across restarts.
pub trait IdentityConfig {
    /// Whether to use ephemeral identity (not persisted).
    ///
    /// Ephemeral identities are generated fresh each run. Suitable for
    /// light nodes and testing, but not for full nodes with storage duties.
    fn ephemeral(&self) -> bool;

    /// Whether this identity needs to be persistent.
    ///
    /// Returns true if the node configuration requires a stable identity
    /// (e.g., for storage nodes, SWAP, redistribution).
    fn requires_persistent(&self) -> bool;
}

/// Combined Swarm protocol configuration.
///
/// This super-trait combines all protocol-level configs into one bound.
/// Useful for builders and contexts that need access to all Swarm settings.
///
/// Any type implementing all the individual config traits automatically
/// implements `SwarmConfig` via the blanket implementation.
pub trait SwarmConfig:
    AvailabilityIncentiveConfig + StoreConfig + StorageConfig + NetworkConfig + IdentityConfig
{
}

impl<T> SwarmConfig for T where
    T: AvailabilityIncentiveConfig + StoreConfig + StorageConfig + NetworkConfig + IdentityConfig
{
}

/// Cache capacity divisor relative to reserve capacity.
///
/// Default cache is `reserve_capacity / CACHE_CAPACITY_DIVISOR`.
/// With divisor of 64: 2^22 / 64 = 2^16 = 65,536 chunks (~256MB).
pub const CACHE_CAPACITY_DIVISOR: u64 = 64;

/// Implement [`StoreConfig`] for any [`SwarmSpec`].
///
/// The spec provides all the information needed:
/// - `capacity_chunks()` returns `spec.reserve_capacity()`
/// - `cache_chunks()` returns `capacity / 64`
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
///
/// This accounts for LevelDB overhead, chunk indexes, stamps, and other
/// metadata stored alongside chunks. A 20% overhead is a reasonable estimate.
pub const METADATA_OVERHEAD_FACTOR: f64 = 0.20;

/// Estimate the total disk space required for storing a given number of chunks.
///
/// This includes both chunk data and metadata overhead.
///
/// # Arguments
/// * `num_chunks` - Number of chunks to store
/// * `chunk_size` - Size of each chunk in bytes (typically 4096)
///
/// # Returns
/// Estimated total bytes required on disk.
///
/// # Example
/// ```
/// use vertex_swarm_api::estimate_storage_bytes;
///
/// // 1 million chunks at 4KB each
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
/// This accounts for metadata overhead, so the returned number of chunks
/// will fit within the specified bytes including all metadata.
///
/// # Arguments
/// * `available_bytes` - Available disk space in bytes
/// * `chunk_size` - Size of each chunk in bytes (typically 4096)
///
/// # Returns
/// Maximum number of chunks that can be stored.
///
/// # Example
/// ```
/// use vertex_swarm_api::estimate_chunks_for_bytes;
///
/// // How many chunks fit in 10 GB?
/// let chunks = estimate_chunks_for_bytes(10 * 1024 * 1024 * 1024, 4096);
/// assert_eq!(chunks, 2_184_533); // ~2.18 million chunks
/// ```
pub fn estimate_chunks_for_bytes(available_bytes: u64, chunk_size: usize) -> u64 {
    // bytes = chunks * chunk_size * (1 + overhead)
    // chunks = bytes / (chunk_size * (1 + overhead))
    let effective_chunk_size = chunk_size as f64 * (1.0 + METADATA_OVERHEAD_FACTOR);
    (available_bytes as f64 / effective_chunk_size) as u64
}
