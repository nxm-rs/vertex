//! The chain-read surface the engine depends on.
//!
//! [`ChainReader`] is the narrow slice of `alloy_provider::Provider` the
//! [`EventEngine`](crate::EventEngine) actually uses: fetch the finalized head,
//! and page logs for a filter. Depending on this slice rather than the full
//! `Provider` trait keeps the engine testable against synthetic logs without a
//! live RPC, while the blanket impl means any real `Provider<Ethereum>` is a
//! `ChainReader` for free.

use alloy_network::Ethereum;
use alloy_primitives::B256;
use alloy_provider::Provider;
use alloy_rpc_types_eth::{BlockNumberOrTag, Filter, Log};

use crate::IndexError;

/// A finalized block's number and hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizedHead {
    /// The finalized block number.
    pub number: u64,
    /// The finalized block hash.
    pub hash: B256,
}

/// The chain reads the engine drives.
///
/// Implemented for every `alloy_provider::Provider<Ethereum>` by the blanket
/// impl below; unit tests supply an in-memory implementation that serves
/// synthetic logs.
#[allow(async_fn_in_trait)]
pub trait ChainReader: Send + Sync {
    /// The current finalized head (number and hash).
    ///
    /// Returns `None` when the chain has not finalized any block yet (a fresh
    /// devnet), in which case the engine has nothing safe to index and waits.
    async fn finalized_head(&self) -> Result<Option<FinalizedHead>, IndexError>;

    /// Fetch all logs matching `filter`.
    ///
    /// The engine sets the filter's block range per page; the implementor just
    /// forwards it to `eth_getLogs`.
    async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, IndexError>;
}

impl<P> ChainReader for P
where
    P: Provider<Ethereum> + Send + Sync,
{
    async fn finalized_head(&self) -> Result<Option<FinalizedHead>, IndexError> {
        let block = self
            .get_block_by_number(BlockNumberOrTag::Finalized)
            .await?;
        Ok(block.map(|b| FinalizedHead {
            number: b.header.number,
            hash: b.header.hash,
        }))
    }

    async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, IndexError> {
        Ok(Provider::get_logs(self, filter).await?)
    }
}
