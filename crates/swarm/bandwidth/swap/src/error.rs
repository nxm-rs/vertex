//! SWAP settlement errors.

/// Errors that can occur during swap operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SwapError {
    /// Service has stopped.
    #[error("swap service stopped")]
    ServiceStopped,

    /// Settlement already in progress with this peer.
    #[error("settlement already in progress")]
    SettlementInProgress,

    /// Network error.
    #[error("network error: {0}")]
    NetworkError(String),

    /// Cheque signing failed.
    #[error("cheque signing failed: {0}")]
    SigningFailed(String),

    /// Chequebook has insufficient balance.
    #[error("insufficient chequebook balance")]
    InsufficientBalance,

    /// Cheque validation failed.
    #[error("cheque validation failed: {0}")]
    ValidationFailed(String),

    /// Chain backend not available.
    #[error("chain backend not available")]
    NoChainBackend,
}
