//! Consumer-facing chequebook chain trait.
//!
//! This is the trait the SWAP settlement service injects. It speaks chequebook
//! semantics (balances, payouts, deploy, cashout) rather than raw calls, so the
//! settlement service never assembles ABI or manages a nonce. The chequebook
//! crate stays a pure cheque codec and consumes this trait; the on-chain
//! implementation lives in the native service crate.

use core::time::Duration;

use alloy_primitives::{Address, B256, TxHash, U256};
use bytes::Bytes;
use vertex_swarm_bandwidth_chequebook::SignedCheque;

use crate::ChainError;

/// On-chain chequebook operations for the SWAP settlement service.
///
/// Read methods take the chequebook contract address explicitly so a single
/// implementation can serve queries against any peer's chequebook, not just the
/// local one. Write methods return a [`TxHash`] for the caller to confirm
/// through the [`crate::TransactionSender`].
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait ChequebookChain: Send + Sync {
    /// Total token balance held by chequebook `chequebook`.
    async fn balance(&self, chequebook: Address) -> Result<U256, ChainError>;

    /// Liquid balance of `chequebook` available to `beneficiary` after hard
    /// deposits to others are reserved.
    async fn liquid_balance_for(
        &self,
        chequebook: Address,
        beneficiary: Address,
    ) -> Result<U256, ChainError>;

    /// Cumulative amount already paid out from `chequebook` to `beneficiary`.
    async fn paid_out(&self, chequebook: Address, beneficiary: Address)
    -> Result<U256, ChainError>;

    /// Deploy a new chequebook for `issuer` via the factory.
    ///
    /// `timeout` is the default hard-deposit timeout; `salt` selects the
    /// CREATE2 address. Returns the deployment transaction hash.
    async fn deploy(
        &self,
        issuer: Address,
        timeout: Duration,
        salt: B256,
    ) -> Result<TxHash, ChainError>;

    /// Cash a cheque where the caller is the beneficiary, paying out to
    /// `recipient`. Uses `cashChequeBeneficiary` with the issuer signature
    /// carried by the cheque.
    async fn cash_cheque_beneficiary(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
    ) -> Result<TxHash, ChainError>;

    /// Cash a cheque on behalf of its beneficiary, paying `payout` to the
    /// caller and the rest to `recipient`. Uses `cashCheque` with both the
    /// beneficiary signature (carried by the cheque) and the issuer/authorizing
    /// signature `sig`.
    async fn cash_cheque(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
        payout: U256,
        sig: Bytes,
    ) -> Result<TxHash, ChainError>;
}
