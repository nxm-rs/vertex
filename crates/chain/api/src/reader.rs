//! Read-only chain access and health probing.

use core::time::Duration;

use alloy_primitives::{Address, Log, U256};
use bytes::Bytes;

use crate::{LogFilter, ProviderError};

/// Read-only view of the chain.
///
/// Everything a consumer needs to observe state without sending a transaction:
/// chain identity, head, balances, `eth_call`, and log queries. Injected as
/// `Arc<dyn ChainReader>` so a service can depend on the read surface alone.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait ChainReader: Send + Sync {
    /// EIP-155 chain id.
    async fn chain_id(&self) -> Result<u64, ProviderError>;

    /// Current head block number.
    async fn block_number(&self) -> Result<u64, ProviderError>;

    /// Timestamp of the given block, or of the head when `block` is `None`.
    async fn block_timestamp(&self, block: Option<u64>) -> Result<u64, ProviderError>;

    /// Native balance of an account at the head.
    async fn balance(&self, address: Address) -> Result<U256, ProviderError>;

    /// Execute a read-only `eth_call` against `to` with `data`, at the given
    /// block or the head when `block` is `None`. Returns the raw return bytes.
    async fn call(
        &self,
        to: Address,
        data: Bytes,
        block: Option<u64>,
    ) -> Result<Bytes, ProviderError>;

    /// Query logs matching `filter`.
    async fn logs(&self, filter: LogFilter) -> Result<Vec<Log>, ProviderError>;
}

/// A [`ChainReader`] that can also report transport sync health.
///
/// Separated from [`ChainReader`] because not every read consumer cares whether
/// the transport is caught up to the network head. A health-gated consumer
/// (redistribution, redistribution agent startup) depends on this; a pure read
/// consumer depends only on [`ChainReader`].
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait ChainHealth: ChainReader {
    /// `true` if the transport's head is within `max_delay` of wall-clock time.
    ///
    /// A node should refuse to participate in time-sensitive on-chain games
    /// (redistribution rounds) while this is `false`.
    async fn is_synced(&self, max_delay: Duration) -> Result<bool, ProviderError>;
}
