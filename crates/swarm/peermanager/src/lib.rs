//! Peer lifecycle management with clean abstraction boundary.
//!
//! This crate provides the bridge layer between:
//! - **libp2p layer** (`vertex-net-*`): Uses `PeerId`, `Multiaddr`, `ConnectionId`
//! - **Swarm layer** (`vertex-client-*`): Uses `OverlayAddress`
//!
//! # Abstraction Boundary
//!
//! All public APIs use `OverlayAddress` only. The `PeerId` mapping is
//! encapsulated internally and exposed only through the [`InternalPeerManager`]
//! trait for use by the bridge layer in `vertex-swarm-client`.
//!
//! # Usage
//!
//! ```ignore
//! use vertex_swarm_peermanager::{PeerManager, InternalPeerManager};
//!
//! let pm = PeerManager::new();
//!
//! // Public API - OverlayAddress only
//! pm.add_known(overlay);
//! pm.start_connecting(overlay);
//! pm.is_connected(&overlay);
//!
//! // Bridge API - PeerId for libp2p integration
//! pm.on_peer_ready(peer_id, overlay, is_full_node);
//! pm.on_peer_disconnected(&peer_id);
//! pm.resolve_peer_id(&overlay);
//! ```

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod manager;
mod state;
mod store;
mod multiaddr_cache;
mod ip_addr;
mod local_network;
mod address_manager;

pub mod discovery;
pub mod score;

pub use address_manager::AddressManager;
pub use ip_addr::{AddressScope, classify_multiaddr, extract_ip, same_subnet};
pub use local_network::{
    LocalNetworkInfo, get_local_network_info, is_directly_reachable, is_on_same_local_network,
    refresh_network_info,
};

pub use discovery::{
    DiscoveryReceiver, DiscoverySender, discovery_channel, run_peer_store_consumer,
};
// Re-export SwarmPeer for convenience (channel uses this type)
pub use vertex_swarm_peer::SwarmPeer;
pub use manager::{FailureReason, InternalPeerManager, PeerManager, PeerManagerStats};
pub use state::{BanInfo, PeerInfo, PeerState, StoredPeer};
pub use store::{FilePeerStore, MemoryPeerStore, PeerStore, PeerStoreError};
