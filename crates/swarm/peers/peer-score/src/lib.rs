//! Swarm-specific peer scoring with configurable policies and callbacks.
//!
//! [`SwarmPeerScore::record_event`] applies the configured weight and returns
//! a [`ScoreOutcome`] computed from the warn, disconnect, and ban thresholds
//! so the caller owns the resulting action. The closure-based
//! [`ScoreCallbacks`] remain as a transitional shim until the peer manager's
//! single report path consumes the returned outcome directly.

#[macro_use]
mod macros;
mod config;
mod score;

pub use config::{SwarmScoringConfig, SwarmScoringConfigBuilder, SwarmScoringEvent};
pub use score::{
    ScoreCallbacks, ScoreChangedFn, ScoreOutcome, ScoreWarningFn, SevereEventFn, ShouldBanFn,
    SwarmPeerScore,
};
pub use vertex_swarm_api::DEFAULT_PEER_DISCONNECT_THRESHOLD;

// Re-export base scoring types
pub use vertex_net_peer_score::PeerScore;
