//! Peer and IP address scoring system for reputation management.
//!
//! This module provides lock-free reputation tracking at two levels:
//!
//! - **Overlay level** ([`PeerScoreState`]): Atomic counters for per-peer behavior
//! - **IP level** ([`IpScore`]): Tracks behavior patterns across overlay changes
//!
//! The dual-level approach addresses scenarios where:
//! - A malicious actor changes their nonce (new overlay, same IP)
//! - A legitimate node roams (same overlay, new IP)
//!
//! # Design
//!
//! The scoring system follows the bandwidth accounting pattern:
//!
//! - **Lock-free atomics** for hot per-peer counters (no mutex contention)
//! - **Arc-wrapped state** created once, shared via cheap `Arc::clone()`
//! - **Double-checked locking** for the central registry (read-fast, write-on-first-access)
//! - **Handle pattern** for vertex-swarm-client to hold per-peer state
//!
//! # Usage
//!
//! ```ignore
//! use vertex_swarm_peermanager::score::{ScoreManager, ScoreHandle};
//!
//! // Create manager (typically one per node)
//! let manager = ScoreManager::new();
//!
//! // Get a handle for a peer (cheap to clone, store in per-peer state)
//! let handle: ScoreHandle = manager.handle_for(overlay);
//!
//! // Clone handle for different protocol handlers (no contention)
//! let retrieval_handle = handle.clone();
//! let pushsync_handle = handle.clone();
//!
//! // Record events (lock-free atomic operations)
//! retrieval_handle.record_connection_success(latency_ms);
//! pushsync_handle.record_chunk_delivered(latency_ms);
//!
//! // Check thresholds
//! if handle.should_ban(manager.config().ban_threshold) {
//!     peer_manager.ban(overlay, Some("Low score".into()));
//! }
//!
//! // Rank candidates for dialing
//! let ranked = manager.rank_overlays(&candidates);
//! ```

mod config;
mod event;
mod handle;
mod ip;
mod manager;
mod peer;

pub use config::{ScoreConfig, ScoreWeights};
pub use event::ScoreEvent;
pub use handle::ScoreHandle;
pub use ip::IpScore;
pub use manager::{ScoreManager, ScoreManagerStats};
pub use peer::{PeerScoreSnapshot, PeerScoreState};
