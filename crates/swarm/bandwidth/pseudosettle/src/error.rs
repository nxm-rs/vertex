//! Pseudosettle settlement errors.

/// Errors that can occur during pseudosettle operations.
#[derive(Debug, Clone, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PseudosettleSettlementError {
    /// Service has stopped.
    #[error("pseudosettle service stopped")]
    ServiceStopped,

    /// Settlement already in progress with this peer.
    #[error("settlement already in progress")]
    SettlementInProgress,

    /// Too soon since last settlement (rate limiting).
    #[error("too soon since last settlement")]
    TooSoon,

    /// Network error.
    #[error("network error: {0}")]
    NetworkError(String),

    /// Peer rejected the settlement.
    #[error("peer rejected settlement")]
    Rejected,
}
