//! Validated local store configuration.

use vertex_swarm_api::SwarmLocalStoreConfig;

/// Default resident memory budget for the client cache, in bytes (64 MiB).
///
/// A content chunk plus its stamp is on the order of 4 KiB, so this holds tens
/// of thousands of chunks while capping memory directly, which is the bound that
/// matters on a mobile or browser client.
pub const DEFAULT_CACHE_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// Default time a cached single-owner chunk stays serveable, in nanoseconds
/// (5 minutes). Past this, a cache hit is treated as expired and the retrieval
/// is forwarded to fetch the latest revision.
pub const DEFAULT_SOC_CACHE_TTL_NS: u64 = 5 * 60 * 1_000_000_000;

/// Validated local store configuration.
#[derive(Debug, Clone)]
pub struct LocalStoreConfig {
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
}

impl LocalStoreConfig {
    /// Create a new local store configuration.
    pub fn new(cache_budget_bytes: u64, soc_cache_ttl: u64) -> Self {
        Self {
            cache_budget_bytes,
            soc_cache_ttl,
        }
    }

    /// Resident memory budget for the cache, in bytes.
    pub fn cache_budget_bytes(&self) -> u64 {
        self.cache_budget_bytes
    }

    /// How long a cached single-owner chunk stays serveable, in nanoseconds.
    pub fn soc_cache_ttl(&self) -> u64 {
        self.soc_cache_ttl
    }
}

impl Default for LocalStoreConfig {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS)
    }
}

impl SwarmLocalStoreConfig for LocalStoreConfig {
    fn cache_budget_bytes(&self) -> u64 {
        self.cache_budget_bytes
    }

    fn soc_cache_ttl(&self) -> u64 {
        self.soc_cache_ttl
    }
}
