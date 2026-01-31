//! Core bandwidth accounting and pricing for bandwidth incentives.
//!
//! This crate provides the foundational components for bandwidth incentives:
//!
//! - **Accounting**: Per-peer balance tracking with pluggable settlement providers
//! - **Pricing**: Proximity-based chunk pricing
//! - **Settlement**: Pluggable providers for pseudosettle and swap
//!
//! # Units
//!
//! All values are in **Accounting Units (AU)**, not bytes or BZZ tokens.
//! Configuration is provided via the [`AccountingConfig`](vertex_swarm_api::AccountingConfig) trait.
//!
//! # Architecture
//!
//! ```text
//! bandwidth-core
//! ├── accounting (per-peer balance tracking + settlement delegation)
//! ├── pricing (proximity-based chunk pricing)
//! └── settlement (SettlementProvider trait)
//!
//! Downstream crates:
//! ├── pseudosettle (PseudosettleProvider)
//! └── swap (SwapProvider)
//! ```
//!
//! # Provider-Based Settlement
//!
//! The [`Accounting`] struct supports different [`BandwidthMode`](vertex_swarm_api::BandwidthMode)
//! configurations through pluggable settlement providers:
//!
//! - **None**: Empty provider list (basic balance tracking only)
//! - **Pseudosettle**: Single pseudosettle provider
//! - **Swap**: Single swap provider
//! - **Both**: Both pseudosettle and swap providers

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod accounting;
mod pricing;
mod settlement;

// Re-export accounting types
pub use accounting::{
    Accounting, AccountingAction, AccountingError, AccountingPeerHandle, CreditAction, DebitAction,
    PeerState,
};

// Re-export pricing types and constants
pub use pricing::{FixedPricer, NoPricer, Pricer};

// Re-export settlement provider types
pub use settlement::{NoSettlement, SettlementProvider};
