//! Storage incentive CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmStorageConfig;

/// Storage incentive configuration (redistribution, postage).
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[command(next_help_heading = "Storage Incentives")]
#[serde(default)]
pub struct StorageIncentiveArgs {
    /// Participate in redistribution.
    #[arg(long)]
    pub redistribution: bool,
}

impl SwarmStorageConfig for StorageIncentiveArgs {
    fn redistribution_enabled(&self) -> bool {
        self.redistribution
    }
}
