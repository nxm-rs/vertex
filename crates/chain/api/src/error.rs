//! Error taxonomy for the chain trait surface.
//!
//! Every enum derives [`strum::IntoStaticStr`] with snake_case serialization so
//! a variant maps directly to a `reason` metric label without a hand-written
//! match. [`TxError`] composes [`ProviderError`] through `#[from]`, and
//! [`ChainError`] is the umbrella over both for call sites that do not care
//! which layer failed.

use alloy_primitives::TxHash;

/// A failure reading from or talking to the chain transport.
///
/// These are produced by the read path ([`crate::ChainReader`],
/// [`crate::ChainHealth`]) and by the lower half of the send path. The variants
/// are transport- and decode-shaped so a consumer can decide whether to retry
/// (`Transport`), give up (`Decode`), or surface a clear operator message
/// (`Disabled`).
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderError {
    /// The transport (RPC connection) failed.
    #[error("chain transport error: {0}")]
    Transport(String),

    /// The chain returned data that could not be decoded into the expected type.
    #[error("failed to decode chain response: {0}")]
    Decode(String),

    /// The requested resource (block, receipt, log range) was not found.
    #[error("chain resource not found: {0}")]
    NotFound(String),

    /// A read or eth_call reverted on-chain.
    #[error("chain call reverted: {0}")]
    Reverted(String),

    /// The chain service is disabled for this node configuration.
    ///
    /// Returned by chain-off code paths (for example a light node, a bootnode,
    /// or a wasm client) so a consumer can distinguish "no chain configured"
    /// from a genuine transport failure.
    #[error("chain service is disabled")]
    Disabled,
}

/// A failure sending or confirming a transaction.
///
/// Wraps [`ProviderError`] for the read-shaped failures that occur while
/// preparing or polling a transaction, and adds the send-specific variants
/// (nonce conflicts, gas estimation, confirmation timeout).
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum TxError {
    /// An underlying provider read failed while sending or confirming.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// Gas could not be estimated, or the estimate was below `min_gas_limit`.
    #[error("gas estimation failed: {0}")]
    GasEstimation(String),

    /// The transaction was rejected by the node before broadcast.
    #[error("transaction rejected: {0}")]
    Rejected(String),

    /// The transaction did not confirm within the allotted window.
    #[error("transaction {hash} not confirmed in time")]
    ConfirmationTimeout {
        /// Hash of the pending transaction.
        hash: TxHash,
    },

    /// The transaction confirmed but reverted on-chain.
    #[error("transaction {hash} reverted")]
    Reverted {
        /// Hash of the reverted transaction.
        hash: TxHash,
    },

    /// No pending transaction matched the supplied hash.
    #[error("no pending transaction for {hash}")]
    NoSuchPending {
        /// Hash that was looked up.
        hash: TxHash,
    },
}

/// Umbrella error over the read and write halves of the chain surface.
///
/// Consumer-facing traits ([`crate::ChequebookChain`]) return this so a single
/// call site can flatten a read or a send failure without naming the layer.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ChainError {
    /// A read or transport-level failure.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// A transaction send or confirmation failure.
    #[error(transparent)]
    Tx(#[from] TxError),
}
