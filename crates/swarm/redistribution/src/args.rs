//! Redistribution (storage incentives) CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmStorageConfig;

/// Redistribution configuration for storage incentives.
///
/// Controls participation in the Swarm redistribution game, which rewards
/// nodes for storing and serving chunks within their neighborhood.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[command(next_help_heading = "Redistribution")]
#[serde(default)]
pub struct RedistributionArgs {
    /// Participate in redistribution (storage incentives).
    ///
    /// When enabled, the node will participate in the redistribution game,
    /// committing storage proofs and potentially earning BZZ rewards.
    #[arg(long)]
    pub redistribution: bool,
}

impl SwarmStorageConfig for RedistributionArgs {
    fn redistribution_enabled(&self) -> bool {
        self.redistribution
    }
}
