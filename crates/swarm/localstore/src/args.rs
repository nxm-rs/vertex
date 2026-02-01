//! Local store CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmLocalStoreConfig;
use vertex_swarmspec::SwarmSpec;

/// Cache divisor for storer nodes (smaller cache relative to reserve).
const STORER_CACHE_DIVISOR: u64 = 64;

/// Local store configuration arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Local Store")]
#[serde(default)]
pub struct LocalStoreArgs {
    /// Cache capacity in number of chunks.
    #[arg(long = "localstore.cache-chunks", default_value_t = vertex_swarmspec::DEFAULT_CACHE_CAPACITY)]
    pub cache_chunks: u64,
}

impl LocalStoreArgs {
    /// Compute cache size for a storer node based on reserve capacity.
    pub fn for_storer(spec: &impl SwarmSpec) -> Self {
        Self {
            cache_chunks: spec.reserve_capacity() / STORER_CACHE_DIVISOR,
        }
    }
}

impl Default for LocalStoreArgs {
    fn default() -> Self {
        Self {
            cache_chunks: vertex_swarmspec::DEFAULT_CACHE_CAPACITY,
        }
    }
}

impl SwarmLocalStoreConfig for LocalStoreArgs {
    fn cache_chunks(&self) -> u64 {
        self.cache_chunks
    }
}
