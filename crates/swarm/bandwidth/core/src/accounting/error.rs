//! Accounting errors (threshold violations, settlement failures).

use vertex_swarm_primitives::OverlayAddress;

/// Errors that can occur during accounting operations.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum AccountingError {
    /// Peer has exceeded disconnect threshold.
    #[error("peer {peer} balance {balance} exceeds disconnect threshold {threshold}")]
    DisconnectThreshold {
        peer: OverlayAddress,
        balance: i64,
        threshold: u64,
    },

    /// Operation would exceed payment threshold.
    #[error("peer {peer} balance {balance} exceeds payment threshold {threshold}")]
    PaymentThreshold {
        peer: OverlayAddress,
        balance: i64,
        threshold: u64,
    },

    /// Peer not found.
    #[error("peer {0} not found")]
    PeerNotFound(OverlayAddress),

    /// Settlement failed.
    #[error("settlement failed: {0}")]
    SettlementFailed(String),

    /// Channel closed (service stopped).
    #[error("channel closed")]
    ChannelClosed,
}

impl AccountingError {
    /// Record this error in metrics.
    pub fn record(&self) {
        let reason: &'static str = self.into();
        metrics::counter!("accounting_errors_total", "reason" => reason).increment(1);
    }
}
