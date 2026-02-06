//! Peer lifecycle management with OverlayAddress/PeerId abstraction boundary.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod ext;
mod manager;

pub use vertex_net_local::{
    AddressScope, IpCapability, IpVersion, LocalCapabilities, LocalSubnets, NetworkCapability,
    TransportCapability, classify_multiaddr, extract_ip, is_directly_reachable, is_ipv4, is_ipv6,
    is_on_same_subnet, query_local_subnets, refresh_subnets, same_subnet,
};

pub use vertex_net_peers::{
    BanInfo, ConnectionState, DEFAULT_BAN_THRESHOLD, DEFAULT_MAX_TRACKED_PEERS, FilePeerStore,
    MemoryPeerStore, NetPeerData, NetPeerId, NetPeerSnapshot, NetPeerStore, NetScoringEvent,
    PeerStoreError,
};
use vertex_swarm_primitives::OverlayAddress;

pub use ext::{SwarmExt, SwarmExtSnapshot};

/// Swarm peer state snapshot for persistence.
pub type PeerSnapshot = NetPeerSnapshot<OverlayAddress, SwarmExtSnapshot, ()>;

/// Swarm-specific peer store trait.
pub type PeerStore = dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>;

pub use manager::{
    InternalPeerManager, PeerManager, PeerManagerStats, PeerReadyResult, SwarmNetPeerManager,
};
pub use vertex_swarm_peer::SwarmPeer;
