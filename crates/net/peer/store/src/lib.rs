//! Peer persistence with generic record storage.

pub mod backoff;
pub mod error;
pub mod file;
pub mod memory;
pub mod traits;

pub use backoff::{BackoffState, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS};
pub use error::StoreError;
pub use file::FilePeerStore;
pub use memory::MemoryPeerStore;
pub use traits::{NetPeerId, NetPeerStore, NetRecord};
