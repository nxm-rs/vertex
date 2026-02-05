//! Peer management with Arc-per-peer pattern for minimal lock contention.
//!
//! This crate provides generic peer state management built on libp2p primitives
//! (PeerId, Multiaddr). It uses an Arc-per-peer pattern where protocol handlers
//! get `Arc<PeerState>` once, then all subsequent operations are lock-free
//! (atomics) or per-peer locked (no global contention).

pub mod manager;
pub mod registry;
pub mod score;
pub mod state;
pub mod store;
pub mod traits;

mod time;

pub use manager::{DEFAULT_BAN_THRESHOLD, DEFAULT_MAX_TRACKED_PEERS, NetPeerManager};
pub use registry::{PeerRegistry, RegisterResult};
pub use score::{PeerScore, PeerScoreSnapshot};
pub use state::{BanInfo, ConnectionState, NetPeerSnapshot, PeerState};
pub use store::{ExtSnapBounds, FilePeerStore, MemoryPeerStore, NetPeerStore, PeerStoreError};
pub use traits::{NetPeerData, NetPeerExt, NetPeerId, NetPeerScoreExt, NetScoringEvent};
