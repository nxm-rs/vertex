//! Swarm-specific peer scoring with configurable policies and callbacks.
//!
//! This crate provides:
//! - [`SwarmScoringEvent`] - Enumeration of Swarm-specific scoring events
//! - [`SwarmScoringConfig`] - Configurable weights for scoring events
//! - [`ScoreObserver`] - Callback trait for score change notifications
//! - [`SwarmPeerScore`] - Wrapper around `PeerScore` with Swarm policy

#[macro_use]
mod macros;
mod callbacks;
mod config;
mod score;

pub use callbacks::{NoOpScoreObserver, ScoreObserver};
pub use config::{SwarmScoringConfig, SwarmScoringConfigBuilder, SwarmScoringEvent};
pub use score::SwarmPeerScore;

// Re-export base scoring types
pub use vertex_net_peer_score::{PeerScore, PeerScoreSnapshot};
