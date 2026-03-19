//! Local store CLI arguments.

use crate::LocalStoreConfig;
use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_spec::SwarmSpec;

/// Cache divisor for storer nodes (smaller cache relative to reserve).
const STORER_CACHE_DIVISOR: u64 = 64;

/// Local store configuration arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Local Store")]
#[serde(default)]
pub struct LocalStoreArgs {
    /// Cache capacity in number of chunks.
    #[arg(long = "localstore.cache-chunks", default_value_t = vertex_swarm_spec::DEFAULT_CACHE_CAPACITY)]
    pub cache_chunks: u64,
}

impl LocalStoreArgs {
    /// Compute cache size for a storer node based on reserve capacity.
    pub fn for_storer(spec: &impl SwarmSpec) -> Self {
        Self {
            cache_chunks: spec.reserve_capacity() / STORER_CACHE_DIVISOR,
        }
    }

    /// Create validated LocalStoreConfig from these CLI arguments.
    pub fn local_store_config(&self) -> LocalStoreConfig {
        LocalStoreConfig::new(self.cache_chunks)
    }
}

impl Default for LocalStoreArgs {
    fn default() -> Self {
        Self {
            cache_chunks: vertex_swarm_spec::DEFAULT_CACHE_CAPACITY,
        }
    }
}
