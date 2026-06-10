//! Swarm-specific peer scoring with configurable threshold policies.
//!
//! [`SwarmPeerScore::record_event`] applies the configured weight and returns
//! a [`ScoreOutcome`] computed from the warn, disconnect, and ban thresholds
//! so the caller owns the resulting action. The peer manager's report path
//! (`PeerManager::report_peer`) is the single consumer that maps outcomes to
//! lifecycle events and side effects; this crate stays policy-only and never
//! executes actions itself.

#[macro_use]
mod macros;
mod config;
mod score;

pub use config::{SwarmScoringConfig, SwarmScoringConfigBuilder, SwarmScoringEvent};
pub use score::{ScoreChange, ScoreOutcome, SwarmPeerScore};
pub use vertex_swarm_api::DEFAULT_PEER_DISCONNECT_THRESHOLD;

// Re-export base scoring types
pub use vertex_net_peer_score::PeerScore;
