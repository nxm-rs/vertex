//! Peer lifecycle management with OverlayAddress/PeerId abstraction boundary.
//!
//! The entire known peer set is held in memory; persistence is an optional
//! identity-only snapshot written periodically and on shutdown. Reputation,
//! bans, and dial backoff are runtime-only and never survive a restart.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod entry;
mod maintenance;
mod manager;
mod proximity_index;
mod score_distribution;
mod scoring;
mod snapshot_store;
mod tasks;

pub use entry::{PeerSnapshot, TrustLevel};
pub use manager::{LIFECYCLE_CHANNEL_CAPACITY, PeerManager, PeerManagerConfig, PeerManagerHandle};
pub use proximity_index::{AddError, ProximityIndex};
pub use score_distribution::ScoreDistribution;
pub use snapshot_store::DbPeerSnapshotStore;
pub use tasks::{DEFAULT_TICK_INTERVAL, spawn_peer_manager_task};
