//! Chain (Ethereum RPC) configuration, errors, and provider extensions.
//!
//! This crate is a thin layer over [`alloy`]. It does not wrap or reimplement an
//! alloy provider: reads, calls, balances, log queries, transaction submission,
//! and confirmation are all done by an `alloy_provider::Provider` with its
//! fillers directly. What this crate adds is the small amount that alloy does not
//! cover for a Swarm node:
//!
//! - [`ChainConfig`]: the contract address book plus the settlement chain. The
//!   network-to-chain mapping comes from [`nectar_swarms`], the addresses from
//!   `nectar_contracts`.
//! - [`ChainError`] / [`TxError`]: typed errors over alloy's transport and
//!   pending-transaction errors, with `strum::IntoStaticStr` `reason` labels.
//! - [`ProviderExt`]: an extension trait on `alloy_provider::Provider` for the
//!   three pending-transaction operations alloy has no built-in for (resend,
//!   cancel, recover_pending).
//! - [`TxRequest`]: a newtype over `alloy_rpc_types_eth::TransactionRequest` that
//!   attaches a static description for logs and metrics.
//!
//! Alloy providers run on `wasm32-unknown-unknown` with the right transport, so
//! this crate stays wasm-compatible by depending on `alloy-provider` with
//! `default-features = false` (no reqwest, no native TLS). The concrete transport
//! is selected by the consumer.
//!
//! [`alloy`]: alloy_provider

mod config;
mod error;
mod provider;
mod tx;

#[cfg(test)]
mod tests;

pub use config::ChainConfig;
pub use error::{ChainError, TxError};
pub use provider::ProviderExt;
pub use tx::TxRequest;
