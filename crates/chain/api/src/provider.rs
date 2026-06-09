//! Pending-transaction operations alloy has no built-in for.
//!
//! Sending, confirming, gas estimation, nonce filling, and fee pricing are all
//! done by an `alloy_provider::Provider` with its fillers: a consumer calls
//! `provider.send_transaction(req).await?.get_receipt().await` and never needs a
//! helper from this crate. What alloy does not offer is replacing a transaction
//! that is already in flight at a known nonce. [`ProviderExt`] adds exactly that,
//! as an extension trait with a blanket impl over every `Provider<Ethereum>`.
//!
//! Recovery of transactions left pending across a process restart is
//! deliberately absent: a `Provider` holds no record of what a previous run
//! broadcast, so that is application-persisted state, not a provider operation.
//! The component that owns that persistence reconstructs the hashes and then
//! uses [`ProviderExt::resend`] or [`ProviderExt::cancel`] on each.

use alloy_network::Ethereum;
use alloy_primitives::{TxHash, U256};
use alloy_provider::{PendingTransactionBuilder, Provider};
use alloy_rpc_types_eth::TransactionRequest;

use crate::TxError;

/// A fee bump expressed as a percentage of the original fee.
///
/// A node bumps the priority fee (and the cap, to keep it above the priority
/// fee) by this much when it replaces a stuck transaction. A replacement that
/// does not raise the fee enough is rejected, so the default leaves comfortable
/// headroom.
const DEFAULT_FEE_BUMP_PERCENT: u128 = 10;

/// Extension trait adding transaction-replacement operations to an alloy
/// [`Provider`].
///
/// Every method operates on a transaction already broadcast at a known nonce.
/// The blanket impl covers any provider over the [`Ethereum`] network, so a
/// consumer writes `provider.resend(hash).await` directly.
#[allow(async_fn_in_trait)]
pub trait ProviderExt: Provider<Ethereum> {
    /// Resubmit a stuck transaction with a higher fee, returning the new pending
    /// transaction.
    ///
    /// Looks the transaction up by `hash`, rebuilds it with a bumped priority
    /// fee at the same nonce, and broadcasts the replacement. The returned hash
    /// differs from `hash` once the fee bump changes the transaction's identity.
    /// Fails with [`TxError::NoSuchPending`] if the node no longer knows the
    /// transaction (it confirmed, or was dropped).
    async fn resend(&self, hash: TxHash) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        let tx = self
            .get_transaction_by_hash(hash)
            .await?
            .ok_or(TxError::NoSuchPending { hash })?;

        // `from_recovered_transaction` keeps the original sender so the
        // replacement lands on the same account and nonce.
        let mut replacement = TransactionRequest::from_recovered_transaction(tx.inner);
        bump_fees(&mut replacement);
        Ok(self.send_transaction(replacement).await?)
    }

    /// Cancel a pending transaction by replacing it with a zero-value self-send
    /// at the same nonce and a higher fee.
    ///
    /// Returns the cancellation's pending transaction. Fails with
    /// [`TxError::NoSuchPending`] if the node no longer knows the transaction.
    async fn cancel(&self, hash: TxHash) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        let tx = self
            .get_transaction_by_hash(hash)
            .await?
            .ok_or(TxError::NoSuchPending { hash })?;

        let sender = tx.inner.signer();

        let mut cancellation = TransactionRequest::from_recovered_transaction(tx.inner);
        // A cancellation is a no-op self-send at the same nonce: keep the sender
        // and nonce, but clear the payload, value, and gas so the provider
        // re-estimates the minimal cost of an empty self-send.
        cancellation.to = Some(sender.into());
        cancellation.value = Some(U256::ZERO);
        cancellation.input = Default::default();
        cancellation.gas = None;
        bump_fees(&mut cancellation);

        Ok(self.send_transaction(cancellation).await?)
    }
}

impl<P> ProviderExt for P where P: Provider<Ethereum> {}

/// Raise the EIP-1559 fees on a replacement request so a node accepts it over
/// the original.
fn bump_fees(request: &mut TransactionRequest) {
    if let Some(tip) = request.max_priority_fee_per_gas {
        request.max_priority_fee_per_gas = Some(bump(tip));
    }
    if let Some(cap) = request.max_fee_per_gas {
        request.max_fee_per_gas = Some(bump(cap));
    }
    if let Some(price) = request.gas_price {
        request.gas_price = Some(bump(price));
    }
}

/// Apply [`DEFAULT_FEE_BUMP_PERCENT`] to a fee value, saturating on overflow.
fn bump(fee: u128) -> u128 {
    fee.saturating_add(fee.saturating_mul(DEFAULT_FEE_BUMP_PERCENT) / 100)
}
