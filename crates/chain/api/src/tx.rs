//! Transaction request and receipt summary types.
//!
//! Gas parameters are typed fields on [`TxRequest`], not values smuggled through
//! a context-carrying side channel. A caller states its requirements (an
//! optional cap, a floor, and a tip boost) up front; the sender implementation
//! is free to estimate, clamp, and price within those bounds.

use alloy_primitives::{Address, B256, TxHash, U256};
use bytes::Bytes;

/// A request to submit a transaction to the chain.
///
/// The sender implementation owns nonce selection, gas estimation, and fee
/// pricing. This type carries only the caller's intent and bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRequest {
    /// Destination address. `None` deploys a contract with `data` as init code.
    pub to: Option<Address>,

    /// Call data (ABI-encoded call, or contract init code when `to` is `None`).
    pub data: Bytes,

    /// Native value (xDAI on Gnosis) to send with the call.
    pub value: U256,

    /// Hard cap on gas. `None` lets the sender use its estimate.
    pub gas_limit: Option<u64>,

    /// Floor on gas. The send fails with [`crate::TxError::GasEstimation`] if the
    /// estimate falls below this, guarding against an under-estimate that would
    /// revert.
    pub min_gas_limit: Option<u64>,

    /// Percentage to boost the priority fee (tip) above the sender's baseline,
    /// for faster inclusion or for replacement transactions.
    pub tip_boost_percent: u16,

    /// Static, human-readable label for logs and metrics (for example
    /// `"chequebook_deploy"`). Kept `&'static str` so it can be used directly as
    /// a metric label without allocation.
    pub description: &'static str,
}

impl TxRequest {
    /// A plain call to `to` with `data`, zero value and no gas overrides.
    pub fn call(to: Address, data: Bytes, description: &'static str) -> Self {
        Self {
            to: Some(to),
            data,
            value: U256::ZERO,
            gas_limit: None,
            min_gas_limit: None,
            tip_boost_percent: 0,
            description,
        }
    }

    /// A contract deployment carrying `init_code` and no recipient.
    pub fn deploy(init_code: Bytes, description: &'static str) -> Self {
        Self {
            to: None,
            data: init_code,
            value: U256::ZERO,
            gas_limit: None,
            min_gas_limit: None,
            tip_boost_percent: 0,
            description,
        }
    }
}

/// Whether a confirmed transaction succeeded or reverted on-chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxStatus {
    /// The transaction executed successfully.
    Success,
    /// The transaction reverted.
    Reverted,
}

impl TxStatus {
    /// `true` if the transaction executed successfully.
    pub fn is_success(self) -> bool {
        matches!(self, TxStatus::Success)
    }
}

/// A confirmed transaction summary.
///
/// A summary, not the full receipt: just what a settlement or redistribution
/// consumer needs to record an outcome and emit metrics. Implementations derive
/// it from the node's full receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxReceipt {
    /// Hash of the confirmed transaction.
    pub tx_hash: TxHash,

    /// Block the transaction was included in.
    pub block_number: u64,

    /// Success or revert.
    pub status: TxStatus,

    /// Gas units actually consumed.
    pub gas_used: u64,

    /// Address of the contract created, if this was a deployment.
    pub contract_address: Option<Address>,
}

impl TxReceipt {
    /// `true` if the transaction executed successfully.
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// A wasm-safe log query.
///
/// A deliberately small filter so the read trait does not pull a provider's
/// rpc-types crate into the wasm cone. Implementations translate this into the
/// transport's native filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    /// Restrict to logs emitted by these addresses. Empty means any address.
    pub addresses: Vec<Address>,

    /// Topic constraints, positional. A `None` slot matches any value in that
    /// position; the inner `Vec` is an OR set for that position.
    pub topics: Vec<Option<Vec<B256>>>,

    /// Inclusive lower block bound. `None` means earliest.
    pub from_block: Option<u64>,

    /// Inclusive upper block bound. `None` means latest.
    pub to_block: Option<u64>,
}

impl LogFilter {
    /// A filter scoped to a single address over an inclusive block range.
    pub fn for_address(address: Address, from_block: u64, to_block: u64) -> Self {
        Self {
            addresses: vec![address],
            topics: Vec::new(),
            from_block: Some(from_block),
            to_block: Some(to_block),
        }
    }
}
