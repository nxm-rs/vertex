//! Peer lifecycle management with OverlayAddress/PeerId abstraction boundary.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod db_store;
mod entry;
mod maintenance;
mod manager;
mod proximity_index;
mod score_distribution;
mod scoring;
mod tasks;
mod write_buffer;

pub use db_store::DbPeerStore;
pub use entry::{StoredPeer, TrustLevel};
pub use manager::{LIFECYCLE_CHANNEL_CAPACITY, PeerManager, PeerManagerConfig, PeerManagerHandle};
pub use proximity_index::{AddError, ProximityIndex};
pub use score_distribution::ScoreDistribution;
pub use tasks::{PersistenceConfig, PurgeConfig, spawn_persistence_task, spawn_purge_task};
