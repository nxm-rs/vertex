//! Error taxonomy for the event-indexing engine.
//!
//! These wrap the underlying transport and storage errors through `#[from]`
//! rather than flattening them into strings: a consumer keeps the full error for
//! matching and logging, and the [`strum::IntoStaticStr`] discriminant gives a
//! `reason` metric label without a hand-written match. This mirrors the
//! `vertex-chain` `ChainError` style.

use alloy_provider::transport::TransportError;
use vertex_storage::DatabaseError;

/// A failure indexing chain events.
///
/// Returned by [`crate::Indexer`] callbacks and by the engine's backfill and
/// follow loops. The transport and storage variants carry the underlying alloy
/// and `vertex-storage` errors whole so a consumer can inspect the RPC error
/// code or the storage failure.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum IndexError {
    /// The RPC transport failed, or the node returned an error response while
    /// fetching logs or the finalized head.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// A read or write against the persisted cursor (or any state an indexer
    /// folds atomically with it) failed.
    #[error(transparent)]
    Storage(#[from] DatabaseError),

    /// A log returned by the provider was missing a field the engine needs to
    /// order or checkpoint it (`block_number` or `log_index`).
    ///
    /// Canonical logs always carry both; a `None` here means the provider
    /// returned a pending log, which the engine never requests.
    #[error("log missing required field {field}")]
    MalformedLog {
        /// The absent field, used as the operator-facing detail.
        field: &'static str,
    },

    /// An [`Indexer`](crate::Indexer) callback (`apply` or `revert`) failed.
    ///
    /// The engine stops the affected indexer's run loop on this so a fold error
    /// does not silently advance the cursor past unapplied state.
    #[error("indexer {indexer}: {message}")]
    Apply {
        /// The failing indexer's [`name`](crate::Indexer::name).
        indexer: &'static str,
        /// The operator-facing failure detail.
        message: String,
    },
}

impl IndexError {
    vertex_metrics::impl_record_error!("chain_index_errors_total");

    /// Build an [`IndexError::Apply`] for a named indexer.
    pub fn apply(indexer: &'static str, message: impl Into<String>) -> Self {
        Self::Apply {
            indexer,
            message: message.into(),
        }
    }
}
