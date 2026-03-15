//! Peer persistence with generic record storage.

pub mod error;
pub mod memory;
pub mod traits;

pub use error::StoreError;
pub use memory::MemoryPeerStore;
pub use traits::{NetPeerId, NetPeerStore, NetRecord};
