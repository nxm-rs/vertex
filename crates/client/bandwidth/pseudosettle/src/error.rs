//! Pseudosettle error types.

/// Errors that can occur during pseudosettle operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PseudosettleError {
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
