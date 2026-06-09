//! Transaction submission, confirmation, and pending-transaction management.

use alloy_primitives::TxHash;

use crate::{TxError, TxReceipt, TxRequest};

/// Submits and manages transactions on behalf of the node.
///
/// One node-wide sender owns nonce ordering, fee pricing, replacement, and
/// recovery of pending transactions across restarts. Services inject it as
/// `Arc<dyn TransactionSender>` and never touch a signer or a nonce directly.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait TransactionSender: Send + Sync {
    /// Broadcast `request` and return its hash without waiting for inclusion.
    async fn send(&self, request: TxRequest) -> Result<TxHash, TxError>;

    /// Wait for `hash` to confirm and return its receipt summary.
    async fn confirm(&self, hash: TxHash) -> Result<TxReceipt, TxError>;

    /// Broadcast `request` and wait for confirmation.
    ///
    /// Default composition of [`Self::send`] then [`Self::confirm`]. Override
    /// only if an implementation can fuse the two more efficiently.
    async fn send_and_confirm(&self, request: TxRequest) -> Result<TxReceipt, TxError> {
        let hash = self.send(request).await?;
        self.confirm(hash).await
    }

    /// Resubmit a stuck transaction with a higher fee, returning the new hash.
    ///
    /// The replacement reuses the original nonce; the returned hash may differ
    /// from `hash` once the fee bump changes the transaction's identity.
    async fn resend(&self, hash: TxHash) -> Result<TxHash, TxError>;

    /// Cancel a pending transaction by replacing it with a zero-value self-send
    /// at the same nonce and a higher fee. Returns the cancellation's hash.
    async fn cancel(&self, hash: TxHash) -> Result<TxHash, TxError>;

    /// Re-attach to transactions left pending by a previous run.
    ///
    /// Called once at startup so the sender can resume monitoring (and, if
    /// needed, re-price) transactions it broadcast before a restart.
    async fn recover_pending(&self) -> Result<(), TxError>;
}
