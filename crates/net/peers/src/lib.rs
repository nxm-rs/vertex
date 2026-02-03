//! Protocol-agnostic peer management with Arc-per-peer pattern for minimal lock contention.

pub mod events;
pub mod manager;
pub mod registry;
pub mod score;
pub mod state;
pub mod store;
pub mod traits;

pub use events::{EventEmitter, PeerEvent};
pub use manager::{NetPeerManager, NetPeerManagerConfig};
pub use registry::{PeerRegistry, RegisterResult};
pub use score::{PeerScore, PeerScoreSnapshot};
pub use state::{BanInfo, ConnectionState, NetPeerSnapshot, PeerState};
pub use store::{ExtSnapBounds, FilePeerStore, MemoryPeerStore, NetPeerStore, PeerStoreError};
pub use traits::{NetPeerData, NetPeerExt, NetPeerId, NetPeerScoreExt, NetScoringEvent};
