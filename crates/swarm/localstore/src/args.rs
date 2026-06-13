//! Local store CLI arguments.

use crate::LocalStoreConfig;
use crate::config::{DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS};
use clap::Args;
use serde::{Deserialize, Serialize};

/// Local store configuration arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Local Store")]
#[serde(default)]
pub struct LocalStoreArgs {
    /// Resident memory budget for the chunk cache, in bytes.
    #[arg(long = "localstore.cache-budget-bytes", default_value_t = DEFAULT_CACHE_BUDGET_BYTES)]
    pub cache_budget_bytes: u64,

    /// How long a cached single-owner chunk stays serveable, in nanoseconds,
    /// measured against the stamp's signed timestamp. Content chunks ignore it.
    #[arg(long = "localstore.soc-cache-ttl-ns", default_value_t = DEFAULT_SOC_CACHE_TTL_NS)]
    pub soc_cache_ttl: u64,
}

impl LocalStoreArgs {
    /// Create validated [`LocalStoreConfig`] from these CLI arguments.
    pub fn local_store_config(&self) -> LocalStoreConfig {
        LocalStoreConfig::new(self.cache_budget_bytes, self.soc_cache_ttl)
    }
}

impl Default for LocalStoreArgs {
    fn default() -> Self {
        Self {
            cache_budget_bytes: DEFAULT_CACHE_BUDGET_BYTES,
            soc_cache_ttl: DEFAULT_SOC_CACHE_TTL_NS,
        }
    }
}
