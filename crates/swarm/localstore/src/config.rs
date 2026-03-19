//! Validated local store configuration.

use vertex_swarm_api::SwarmLocalStoreConfig;

/// Validated local store configuration.
#[derive(Debug, Clone)]
pub struct LocalStoreConfig {
    cache_chunks: u64,
}

impl LocalStoreConfig {
    /// Create a new local store configuration.
    pub fn new(cache_chunks: u64) -> Self {
        Self { cache_chunks }
    }

    /// Cache capacity in number of chunks.
    pub fn cache_chunks(&self) -> u64 {
        self.cache_chunks
    }
}

impl SwarmLocalStoreConfig for LocalStoreConfig {
    fn cache_chunks(&self) -> u64 {
        self.cache_chunks
    }
}
