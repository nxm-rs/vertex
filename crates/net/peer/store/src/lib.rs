//! Peer persistence with generic record storage.

pub mod error;
pub mod file;
pub mod memory;
pub mod record;
pub mod traits;

pub use error::StoreError;
pub use file::FilePeerStore;
pub use memory::MemoryPeerStore;
pub use record::PeerRecord;
pub use traits::{DataBounds, NetPeerId, NetPeerStore};
