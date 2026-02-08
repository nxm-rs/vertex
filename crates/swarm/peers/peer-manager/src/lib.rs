//! Peer lifecycle management with OverlayAddress/PeerId abstraction boundary.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod ban;
mod data;
mod entry;
mod error;
mod manager;
mod snapshot;

pub use ban::BanInfo;
pub use error::PeerManagerError;

// Re-export from vertex-net-local for address utilities
pub use vertex_net_local::{
    AddressScope, IpCapability, IpVersion, LocalCapabilities, LocalSubnets, NetworkCapability,
    TransportCapability, classify_multiaddr, extract_ip, is_directly_reachable, is_ipv4, is_ipv6,
    is_on_same_subnet, query_local_subnets, refresh_subnets, same_subnet,
};

// Re-export from peer-store for persistence
pub use vertex_net_peer_store::{
    DataBounds, FilePeerStore, MemoryPeerStore, NetPeerId, NetPeerStore, PeerRecord,
    StoreError as PeerStoreError,
};

// Re-export from peer-score for scoring types
pub use vertex_net_peer_score::{PeerScore, PeerScoreSnapshot};

// Re-export Swarm-specific scoring types
pub use vertex_swarm_peer_score::{
    NoOpScoreObserver, ScoreObserver, SwarmPeerScore, SwarmScoringConfig, SwarmScoringEvent,
};

pub use data::{SwarmPeerData, SwarmPeerDataSnapshot};
pub use entry::PeerEntry;
pub use manager::{
    DEFAULT_MAX_TRACKED_PEERS, InternalPeerManager, PeerManager, PeerManagerStats,
};
pub use snapshot::SwarmPeerSnapshot;
pub use vertex_swarm_peer::SwarmPeer;
