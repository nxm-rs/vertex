//! Current info about the Swarm network state

use alloy_primitives::{BlockNumber, B256};
use serde::{Deserialize, Serialize};

/// Current status of the Swarm network.
#[derive(Default, Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SwarmInfo {
    /// The block hash of the highest fully synced block.
    pub best_hash: B256,
    /// The block number of the highest fully synced block.
    pub best_number: BlockNumber,
    /// The current network size estimation
    pub network_size: u64,
    /// Current global postage price per chunk
    pub global_postage_price: u64,
    /// Neighborhood radius (radius of proximity)
    pub neighborhood_radius: u8,
    /// Connected peer count
    pub connected_peers: u16,
}
