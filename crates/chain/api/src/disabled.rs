//! A zero-implementation chain for chain-off node configurations.
//!
//! [`DisabledChain`] implements every chain trait by returning the `Disabled`
//! error. A light node, a bootnode, or a wasm client can inject it as
//! `Arc<dyn ChainReader>` (and the rest) so consumers wired for the chain
//! surface compile and run with a clear, typed "no chain" answer instead of a
//! panic or an `Option` threaded through every call. It is pure (no provider),
//! so it lives in the wasm-safe api crate.

use core::time::Duration;

use alloy_primitives::{Address, B256, Log, TxHash, U256};
use bytes::Bytes;

use crate::{
    ChainError, ChainHealth, ChainReader, ChequebookChain, LogFilter, ProviderError, SignedCheque,
    TransactionSender, TxError, TxReceipt, TxRequest,
};

/// Chain implementation that reports the chain as disabled for every operation.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct DisabledChain;

impl DisabledChain {
    /// Construct the disabled chain.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ChainReader for DisabledChain {
    async fn chain_id(&self) -> Result<u64, ProviderError> {
        Err(ProviderError::Disabled)
    }

    async fn block_number(&self) -> Result<u64, ProviderError> {
        Err(ProviderError::Disabled)
    }

    async fn block_timestamp(&self, _block: Option<u64>) -> Result<u64, ProviderError> {
        Err(ProviderError::Disabled)
    }

    async fn balance(&self, _address: Address) -> Result<U256, ProviderError> {
        Err(ProviderError::Disabled)
    }

    async fn call(
        &self,
        _to: Address,
        _data: Bytes,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        Err(ProviderError::Disabled)
    }

    async fn logs(&self, _filter: LogFilter) -> Result<Vec<Log>, ProviderError> {
        Err(ProviderError::Disabled)
    }
}

#[async_trait::async_trait]
impl ChainHealth for DisabledChain {
    async fn is_synced(&self, _max_delay: Duration) -> Result<bool, ProviderError> {
        Err(ProviderError::Disabled)
    }
}

#[async_trait::async_trait]
impl TransactionSender for DisabledChain {
    async fn send(&self, _request: TxRequest) -> Result<TxHash, TxError> {
        Err(ProviderError::Disabled.into())
    }

    async fn confirm(&self, _hash: TxHash) -> Result<TxReceipt, TxError> {
        Err(ProviderError::Disabled.into())
    }

    async fn resend(&self, _hash: TxHash) -> Result<TxHash, TxError> {
        Err(ProviderError::Disabled.into())
    }

    async fn cancel(&self, _hash: TxHash) -> Result<TxHash, TxError> {
        Err(ProviderError::Disabled.into())
    }

    async fn recover_pending(&self) -> Result<(), TxError> {
        Err(ProviderError::Disabled.into())
    }
}

#[async_trait::async_trait]
impl ChequebookChain for DisabledChain {
    async fn balance(&self, _chequebook: Address) -> Result<U256, ChainError> {
        Err(ProviderError::Disabled.into())
    }

    async fn liquid_balance_for(
        &self,
        _chequebook: Address,
        _beneficiary: Address,
    ) -> Result<U256, ChainError> {
        Err(ProviderError::Disabled.into())
    }

    async fn paid_out(
        &self,
        _chequebook: Address,
        _beneficiary: Address,
    ) -> Result<U256, ChainError> {
        Err(ProviderError::Disabled.into())
    }

    async fn deploy(
        &self,
        _issuer: Address,
        _timeout: Duration,
        _salt: B256,
    ) -> Result<TxHash, ChainError> {
        Err(ProviderError::Disabled.into())
    }

    async fn cash_cheque_beneficiary(
        &self,
        _cheque: &SignedCheque,
        _recipient: Address,
    ) -> Result<TxHash, ChainError> {
        Err(ProviderError::Disabled.into())
    }

    async fn cash_cheque(
        &self,
        _cheque: &SignedCheque,
        _recipient: Address,
        _payout: U256,
        _sig: Bytes,
    ) -> Result<TxHash, ChainError> {
        Err(ProviderError::Disabled.into())
    }
}
