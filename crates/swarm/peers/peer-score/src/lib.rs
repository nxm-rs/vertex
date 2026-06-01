//! Swarm-specific peer scoring with configurable policies and callbacks.

#[macro_use]
mod macros;
mod config;
mod score;
mod stabilization;

pub use config::{SwarmScoringConfig, SwarmScoringConfigBuilder, SwarmScoringEvent};
pub use score::{
    ScoreCallbacks, ScoreChangedFn, ScoreWarningFn, SevereEventFn, ShouldBanFn, SwarmPeerScore,
};
pub use stabilization::{
    ConsecutiveOkDetector, DEFAULT_REQUIRED_OK, DEFAULT_WINDOW, StabilizationConfig,
    StabilizationDetector,
};

// Re-export base scoring types
pub use vertex_net_peer_score::PeerScore;
