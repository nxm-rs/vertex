//! Swarm-specific peer scoring with configurable policies and callbacks.

#[macro_use]
mod macros;
mod config;
mod score;

pub use config::{SwarmScoringConfig, SwarmScoringConfigBuilder, SwarmScoringEvent};
pub use score::{ScoreCallbacks, SwarmPeerScore};

// Re-export base scoring types
pub use vertex_net_peer_score::PeerScore;
