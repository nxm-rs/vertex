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
//! trait for use by the bridge layer in `vertex-client-core`.
//!
//! # Usage
//!
//! ```ignore
//! use vertex_client_peermanager::{PeerManager, InternalPeerManager};
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

mod events;
mod manager;
mod state;
mod store;
mod underlay;

pub mod discovery;

pub use discovery::{
    DiscoveredPeer, DiscoveryReceiver, DiscoverySender, discovery_channel, run_peer_store_consumer,
};
pub use events::PeerManagerEvent;
pub use manager::{InternalPeerManager, PeerManager, PeerManagerStats};
pub use state::{BanInfo, PeerInfo, PeerState, StoredPeer};
pub use store::{FilePeerStore, MemoryPeerStore, PeerStore, PeerStoreError};
