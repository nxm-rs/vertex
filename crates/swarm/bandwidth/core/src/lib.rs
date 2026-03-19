//! Core bandwidth accounting for Swarm.
//!
//! Per-peer balance tracking with pluggable settlement providers.
//! All values are in **Accounting Units (AU)**, not bytes or BZZ tokens.
//!
//! # Components
//!
//! - [`Accounting`] - Per-peer balance factory with settlement delegation
//! - [`AccountingBuilder`] - Builder for constructing accounting with pricing
//! - [`AccountingPeerHandle`] - Handle for recording bandwidth per peer
//! - [`ReceiveAction`] / [`ProvideAction`] - Prepare/apply pattern for balance changes
//! - [`NoSettlement`] - No-op settlement provider
//!
//! Settlement providers (`PseudosettleProvider`, `SwapProvider`) are in sibling crates.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod accounting;
pub mod args;
mod builder;
mod client_accounting;
mod config;
mod constants;
mod noop;
mod settlement;

pub use accounting::{
    Accounting, AccountingAction, AccountingError, AccountingPeerHandle, PeerState,
    PeerStateSnapshot, ProvideAction, ReceiveAction,
};
pub use args::{BandwidthArgs, BandwidthModeArg};
pub use builder::{AccountingBuilder, NoAccountingBuilder};
pub use client_accounting::ClientAccounting;
pub use config::{BandwidthConfig, BandwidthConfigError, DefaultBandwidthConfig};
pub use noop::{NoAccounting, NoPeerBandwidth, NoProvideAction, NoReceiveAction};
pub use settlement::NoSettlement;
pub use vertex_swarm_bandwidth_pricing::{FixedPricer, FixedPricingConfig, NoPricer, Pricer};
