//! Peer lifecycle management with OverlayAddress/PeerId abstraction boundary.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod ban;
mod data;
mod entry;
mod manager;
mod proximity_index;
mod pruner;

pub use data::SwarmPeerData;
pub use manager::{PeerManager, PeerManagerStats};
pub use proximity_index::{ProximityIndex, ProximityIndexCacheStats};
pub use pruner::{PruneConfig, spawn_prune_task};

pub use vertex_swarm_peer_score::SwarmScoringConfig;

/// Histogram bucket configurations for peer manager metrics.
pub const HISTOGRAM_BUCKETS: &[vertex_observability::HistogramBucketConfig] = &[
    vertex_observability::HistogramBucketConfig {
        suffix: "peer_manager_score_distribution",
        buckets: vertex_observability::PEER_SCORE,
    },
];
