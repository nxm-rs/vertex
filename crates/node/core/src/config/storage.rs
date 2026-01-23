//! Storage configuration for TOML persistence.

use serde::{Deserialize, Serialize};

/// Storage configuration (TOML-serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Storage capacity in number of chunks.
    #[serde(default = "default_capacity_chunks")]
    pub capacity_chunks: u64,

    /// Whether to participate in redistribution.
    #[serde(default)]
    pub redistribution: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            capacity_chunks: vertex_swarmspec::DEFAULT_RESERVE_CAPACITY,
            redistribution: false,
        }
    }
}

fn default_capacity_chunks() -> u64 {
    vertex_swarmspec::DEFAULT_RESERVE_CAPACITY
}
