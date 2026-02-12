//! Swarm-specific peer scoring with configurable policies and callbacks.
//!
//! This crate provides:
//! - [`SwarmScoringEvent`] - Enumeration of Swarm-specific scoring events
//! - [`SwarmScoringConfig`] - Configurable weights for scoring events
//! - [`ScoreObserver`] - Callback trait for score change notifications
//! - [`SwarmPeerScore`] - Wrapper around generic `PeerScore` with Swarm policy

mod callbacks;
mod config;
mod events;
mod score;

pub use callbacks::{NoOpScoreObserver, ScoreObserver};
pub use config::{SwarmScoringConfig, SwarmScoringConfigBuilder};
pub use events::SwarmScoringEvent;
pub use score::SwarmPeerScore;

// Re-export generic scoring types
pub use vertex_net_peer_score::{PeerScore, PeerScoreSnapshot, ScoringPolicy};
