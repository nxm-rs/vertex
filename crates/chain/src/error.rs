//! Error taxonomy for chain access.
//!
//! These wrap alloy's own error types through `#[from]` rather than flattening
//! them into strings: a consumer keeps the full transport or
//! pending-transaction error for matching and logging, and the
//! [`strum::IntoStaticStr`] discriminant gives a `reason` metric label without a
//! hand-written match.

use alloy_provider::{PendingTransactionError, transport::TransportError};

/// A failure reading from or talking to the chain transport.
///
/// Produced by read-path helpers and by the lower half of the send path. The
/// alloy [`TransportError`] is carried whole so a consumer can inspect the RPC
/// error code or the underlying transport failure.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ChainError {
    /// The RPC transport failed, or the node returned an error response.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// A contract call returned data that did not decode to the expected type.
    #[error(transparent)]
    Abi(#[from] alloy_sol_types::Error),
}

/// A failure sending, confirming, or replacing a transaction.
///
/// Wraps alloy's [`TransportError`] for the submission half and
/// [`PendingTransactionError`] for the confirmation half, plus the
/// pending-transaction operations this crate adds in [`crate::ProviderExt`].
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum TxError {
    /// The transaction could not be submitted to the node.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// Watching or confirming a pending transaction failed.
    #[error(transparent)]
    Pending(#[from] PendingTransactionError),

    /// No pending transaction matched the supplied hash.
    #[error("no pending transaction for {hash}")]
    NoSuchPending {
        /// Hash that was looked up.
        hash: alloy_primitives::TxHash,
    },
}
