//! Configuration traits for Swarm node components.
//!
//! These traits define the configuration parameters that component builders need.
//! CLI args, config files, or programmatic configs can implement these traits,
//! allowing builders to work with any configuration source.
//!
//! # Design
//!
//! Following the reth pattern:
//! - Traits define *what* configuration is needed
//! - CLI args implement the traits directly (no intermediate structs)
//! - Builders receive `impl ConfigTrait` and extract what they need
//!
//! # Example
//!
//! ```ignore
//! // CLI args implement config traits
//! impl BandwidthIncentiveConfig for BandwidthArgs {
//!     fn pseudosettle_enabled(&self) -> bool { ... }
//!     fn payment_threshold(&self) -> u64 { self.payment_threshold }
//! }
//!
//! // Builder uses the trait
//! fn build_accounting(config: &impl BandwidthIncentiveConfig) -> impl BandwidthAccounting {
//!     if config.pseudosettle_enabled() {
//!         PseudosettleAccounting::new(config.payment_threshold())
//!     } else {
//!         NoBandwidthIncentives
//!     }
//! }
//! ```

use core::time::Duration;

// ============================================================================
// Bandwidth Incentive Configuration
// ============================================================================

/// Configuration for bandwidth incentives (pseudosettle / SWAP).
///
/// Implementations provide the parameters needed to configure bandwidth
/// accounting between peers.
pub trait BandwidthIncentiveConfig {
    /// Whether pseudosettle (soft accounting) is enabled.
    fn pseudosettle_enabled(&self) -> bool;

    /// Whether SWAP (real payment channels) is enabled.
    fn swap_enabled(&self) -> bool;

    /// Payment threshold in bytes.
    ///
    /// When a peer's debt reaches this threshold, settlement is requested.
    fn payment_threshold(&self) -> u64;

    /// Payment tolerance in bytes.
    ///
    /// Additional allowance beyond threshold before refusing service.
    fn payment_tolerance(&self) -> u64;

    /// Disconnect threshold in bytes.
    ///
    /// When a peer's unpaid debt exceeds this, the connection is dropped.
    fn disconnect_threshold(&self) -> u64;

    /// Price per chunk in base units.
    fn price_per_chunk(&self) -> u64;

    /// Check if any bandwidth incentive is enabled.
    fn is_enabled(&self) -> bool {
        self.pseudosettle_enabled() || self.swap_enabled()
    }
}

// ============================================================================
// Storage Configuration
// ============================================================================

/// Configuration for local chunk storage.
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
pub trait StorageConfig {
    /// Maximum storage capacity in number of chunks.
    ///
    /// This determines how many chunks the node can store (Full/Staker nodes).
    /// The actual disk space required will be approximately
    /// `chunks * chunk_size * 1.2` to account for metadata overhead.
    fn capacity_chunks(&self) -> u64;

    /// Cache capacity in number of chunks.
    ///
    /// This is an in-memory cache for frequently accessed chunks, used by
    /// non-storage nodes (Light/Publisher) for retrieval and pushsync.
    /// Ephemeral nodes can use memory-only caching with no disk persistence.
    fn cache_chunks(&self) -> u64;
}

/// Configuration for storage incentives (redistribution, postage).
pub trait StorageIncentiveConfig {
    /// Whether this node participates in redistribution.
    ///
    /// When enabled, the node participates in the redistribution game to earn
    /// rewards for storing chunks in its neighborhood.
    fn redistribution_enabled(&self) -> bool;
}

// ============================================================================
// Network Configuration
// ============================================================================

/// Configuration for P2P networking.
///
/// Implementations provide the parameters needed to configure the libp2p
/// swarm and peer connections.
pub trait NetworkConfig {
    /// Iterator over listen addresses (as string multiaddrs).
    type ListenAddrs<'a>: Iterator<Item = &'a str>
    where
        Self: 'a;

    /// Iterator over bootnode addresses (as string multiaddrs).
    type Bootnodes<'a>: Iterator<Item = &'a str>
    where
        Self: 'a;

    /// Get the listen addresses.
    fn listen_addrs(&self) -> Self::ListenAddrs<'_>;

    /// Get the bootnode addresses.
    fn bootnodes(&self) -> Self::Bootnodes<'_>;

    /// Whether peer discovery is enabled.
    fn discovery_enabled(&self) -> bool;

    /// Maximum number of peer connections.
    fn max_peers(&self) -> usize;

    /// Connection idle timeout.
    fn idle_timeout(&self) -> Duration;
}

// ============================================================================
// API Configuration
// ============================================================================

/// Configuration for HTTP/gRPC API servers.
pub trait ApiConfig {
    /// Whether the HTTP API is enabled.
    fn http_enabled(&self) -> bool;

    /// HTTP API listen address.
    fn http_addr(&self) -> &str;

    /// HTTP API listen port.
    fn http_port(&self) -> u16;

    /// Whether the metrics endpoint is enabled.
    fn metrics_enabled(&self) -> bool;

    /// Metrics listen address.
    fn metrics_addr(&self) -> &str;

    /// Metrics listen port.
    fn metrics_port(&self) -> u16;
}

// ============================================================================
// Identity Configuration
// ============================================================================

/// Configuration for node identity management.
pub trait IdentityConfig {
    /// Whether to use ephemeral identity (not persisted).
    fn ephemeral(&self) -> bool;

    /// Whether this identity needs to be persistent.
    ///
    /// Returns true if the node configuration requires a stable identity
    /// (e.g., for storage nodes, SWAP, redistribution).
    fn requires_persistent(&self) -> bool;
}

// ============================================================================
// Combined Node Configuration
// ============================================================================

/// Complete node configuration combining all component configs.
///
/// This trait provides access to all configuration sections. It's typically
/// implemented by a struct that holds references to individual config sources.
pub trait NodeConfiguration {
    /// Bandwidth incentive configuration.
    type Bandwidth: BandwidthIncentiveConfig;
    /// Storage configuration.
    type Storage: StorageConfig;
    /// Network configuration.
    type Network: NetworkConfig;
    /// API configuration.
    type Api: ApiConfig;
    /// Identity configuration.
    type Identity: IdentityConfig;

    /// Get bandwidth configuration.
    fn bandwidth(&self) -> &Self::Bandwidth;

    /// Get storage configuration.
    fn storage(&self) -> &Self::Storage;

    /// Get network configuration.
    fn network(&self) -> &Self::Network;

    /// Get API configuration.
    fn api(&self) -> &Self::Api;

    /// Get identity configuration.
    fn identity(&self) -> &Self::Identity;
}

// ============================================================================
// Default Implementations
// ============================================================================

/// Default bandwidth configuration (pseudosettle only, standard thresholds).
#[derive(Debug, Clone, Copy)]
pub struct DefaultBandwidthConfig;

impl BandwidthIncentiveConfig for DefaultBandwidthConfig {
    fn pseudosettle_enabled(&self) -> bool {
        true
    }

    fn swap_enabled(&self) -> bool {
        false
    }

    fn payment_threshold(&self) -> u64 {
        10 * 1024 * 1024 * 1024 // 10 GB
    }

    fn payment_tolerance(&self) -> u64 {
        1024 * 1024 * 1024 // 1 GB
    }

    fn disconnect_threshold(&self) -> u64 {
        100 * 1024 * 1024 * 1024 // 100 GB
    }

    fn price_per_chunk(&self) -> u64 {
        10
    }
}

/// No bandwidth incentives configuration.
#[derive(Debug, Clone, Copy)]
pub struct NoBandwidthConfig;

impl BandwidthIncentiveConfig for NoBandwidthConfig {
    fn pseudosettle_enabled(&self) -> bool {
        false
    }

    fn swap_enabled(&self) -> bool {
        false
    }

    fn payment_threshold(&self) -> u64 {
        0
    }

    fn payment_tolerance(&self) -> u64 {
        0
    }

    fn disconnect_threshold(&self) -> u64 {
        u64::MAX
    }

    fn price_per_chunk(&self) -> u64 {
        0
    }
}

/// Default storage configuration.
///
/// Uses protocol defaults from swarmspec:
/// - Capacity: `DEFAULT_RESERVE_CAPACITY` (2^22 chunks, ~20GB with metadata)
/// - Cache: `DEFAULT_CACHE_CAPACITY` (2^16 chunks, ~256MB in memory)
///
/// Full nodes may use `DEFAULT_RESERVE_CAPACITY` for cache as well.
#[derive(Debug, Clone, Copy)]
pub struct DefaultStorageConfig;

impl StorageConfig for DefaultStorageConfig {
    fn capacity_chunks(&self) -> u64 {
        vertex_swarmspec::DEFAULT_RESERVE_CAPACITY
    }

    fn cache_chunks(&self) -> u64 {
        vertex_swarmspec::DEFAULT_CACHE_CAPACITY
    }
}

/// Default storage incentive configuration (redistribution disabled).
#[derive(Debug, Clone, Copy)]
pub struct DefaultStorageIncentiveConfig;

impl StorageIncentiveConfig for DefaultStorageIncentiveConfig {
    fn redistribution_enabled(&self) -> bool {
        false
    }
}

// ============================================================================
// Storage Estimation Utilities
// ============================================================================

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

