//! Wasm-safe chain (Ethereum RPC) trait surface for Vertex.
//!
//! This crate defines *what* a chain service offers, never *how* it talks to a
//! node. It is traits and plain data only: no `alloy-provider`, no
//! `alloy-contract`, no transport, no tokio net features. That keeps it inside
//! the wasm cone so a light client (the default node, including
//! `wasm32-unknown-unknown`) can depend on the chain trait surface without
//! pulling a native RPC stack.
//!
//! # The two-crate split
//!
//! The chain is a node-wide service, not a feature of any one consumer. It
//! splits across two crates:
//!
//! - `vertex-chain-api` (this crate): wasm-safe traits and data. Consumers and
//!   the node builder depend only on this.
//! - `vertex-chain-service` (later, native-only): the `alloy-provider`-backed
//!   implementations of these traits. It is an optional dependency of the
//!   binary and the storer builder, never of a library crate.
//!
//! Cone purity is enforced by the crate boundary: because the provider lives in
//! a separate crate that library code does not name, the default light-node and
//! wasm builds cannot accidentally pull it in.
//!
//! # Traits
//!
//! - [`ChainReader`]: read-only chain access (id, head, balances, `eth_call`,
//!   logs). Injectable as `Arc<dyn ChainReader>`.
//! - [`ChainHealth`]: a [`ChainReader`] that also reports transport sync state.
//! - [`TransactionSender`]: submit, confirm, replace, cancel, and recover
//!   transactions through one node-wide sender.
//! - [`ChequebookChain`]: chequebook semantics for the SWAP settlement service.
//!
//! # Data
//!
//! - [`TxRequest`] / [`TxReceipt`]: a transaction's intent and its confirmed
//!   summary. Gas bounds are typed fields, not side-channel context values.
//! - [`LogFilter`]: a small, transport-agnostic log query.
//! - [`ChainConfig`]: contract addresses and chain id.
//!
//! [`DisabledChain`] implements every trait by returning
//! [`ProviderError::Disabled`], for chain-off node configurations.
//!
//! The chequebook crate stays a pure cheque codec; it consumes
//! [`ChequebookChain`] and never embeds a provider.

mod chequebook;
mod config;
mod disabled;
mod error;
mod reader;
mod sender;
mod tx;

#[cfg(test)]
mod tests;

pub use chequebook::ChequebookChain;
pub use config::ChainConfig;
pub use disabled::DisabledChain;
pub use error::{ChainError, ProviderError, TxError};
pub use reader::{ChainHealth, ChainReader};
pub use sender::TransactionSender;
pub use tx::{LogFilter, TxReceipt, TxRequest, TxStatus};

// Re-export the cheque type the chequebook trait consumes so callers depend on
// one canonical path.
pub use vertex_swarm_bandwidth_chequebook::SignedCheque;
