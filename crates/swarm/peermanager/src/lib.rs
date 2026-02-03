//! Peer lifecycle management with OverlayAddress/PeerId abstraction boundary.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod ext;
mod ip_tracker;
mod manager;

pub mod discovery;
pub mod score;

pub use vertex_net_peer::{
    AddressManager, AddressScope, IpCapability, IpVersion, LocalNetworkInfo, classify_multiaddr,
    extract_ip, get_local_network_info, ip_version, is_directly_reachable, is_ipv4, is_ipv6,
    is_on_same_local_network, same_subnet,
};

pub use vertex_net_peers::{
    BanInfo, ConnectionState, FilePeerStore, MemoryPeerStore, NetPeerData, NetPeerId,
    NetPeerManagerConfig, NetPeerSnapshot, NetPeerStore, NetScoringEvent, PeerStoreError,
};
use vertex_swarm_primitives::OverlayAddress;

pub use ext::{SwarmExt, SwarmExtSnapshot};

/// Swarm peer state snapshot for persistence.
pub type PeerSnapshot = NetPeerSnapshot<OverlayAddress, SwarmExtSnapshot, ()>;

/// Swarm-specific peer store trait.
pub type PeerStore = dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>;

pub use discovery::{
    DiscoveryReceiver, DiscoverySender, discovery_channel, run_peer_store_consumer,
};
pub use ip_tracker::{IpScoreTracker, IpTrackerConfig, IpTrackerStats};
pub use manager::{
    FailureReason, InternalPeerManager, PeerManager, PeerManagerConfig, PeerManagerStats,
    PeerReadyResult, SwarmNetPeerManager,
};
pub use vertex_swarm_peer::SwarmPeer;
