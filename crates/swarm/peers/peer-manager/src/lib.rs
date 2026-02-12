//! Peer lifecycle management with OverlayAddress/PeerId abstraction boundary.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod ban;
mod entry;
mod error;
mod manager;
mod pruner;
mod snapshot;

pub use ban::BanInfo;
pub use error::PeerManagerError;
pub use manager::{DEFAULT_MAX_TRACKED_PEERS, PeerManager, PeerManagerStats};
pub use pruner::{PruneConfig, spawn_prune_task};
pub use snapshot::SwarmPeerSnapshot;

// Re-export essential dependencies
pub use vertex_net_peer_store::{NetPeerStore, PeerRecord, StoreError as PeerStoreError};
pub use vertex_swarm_peer::SwarmPeer;
pub use vertex_swarm_peer_score::SwarmScoringConfig;
pub use vertex_swarm_primitives::SwarmNodeType;
