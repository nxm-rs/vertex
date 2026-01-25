//! Storage and cache CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::{StorageConfig, StoreConfig};

/// Local storage and cache configuration.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Local Storage / Cache")]
#[serde(default)]
pub struct StorageArgs {
    /// Maximum storage capacity in number of chunks.
    ///
    /// Storage in Swarm is measured in chunks (typically 4KB each).
    /// Default is 2^22 chunks (~20GB with metadata).
    #[arg(long = "storage.chunks", default_value_t = vertex_swarmspec::DEFAULT_RESERVE_CAPACITY)]
    pub capacity_chunks: u64,

    /// Cache capacity in number of chunks.
    ///
    /// In-memory cache for frequently accessed chunks (Light/Publisher nodes).
    /// Default is 2^16 chunks (~256MB in memory).
    #[arg(long = "cache.chunks", default_value_t = vertex_swarmspec::DEFAULT_CACHE_CAPACITY)]
    pub cache_chunks: u64,
}

impl Default for StorageArgs {
    fn default() -> Self {
        Self {
            capacity_chunks: vertex_swarmspec::DEFAULT_RESERVE_CAPACITY,
            cache_chunks: vertex_swarmspec::DEFAULT_CACHE_CAPACITY,
        }
    }
}

impl StoreConfig for StorageArgs {
    fn capacity_chunks(&self) -> u64 {
        self.capacity_chunks
    }

    fn cache_chunks(&self) -> u64 {
        self.cache_chunks
    }
}

/// Storage incentive configuration (redistribution, postage).
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[command(next_help_heading = "Storage Incentives")]
#[serde(default)]
pub struct StorageIncentiveArgs {
    /// Participate in redistribution (requires persistent identity and staking).
    ///
    /// When enabled, the node participates in the redistribution game to earn
    /// rewards for storing chunks in its neighborhood.
    #[arg(long)]
    pub redistribution: bool,
}

impl StorageConfig for StorageIncentiveArgs {
    fn redistribution_enabled(&self) -> bool {
        self.redistribution
    }
}
